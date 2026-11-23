//! Putting the parts a network reads back together into the symbols that
//! drew them — the way back from a table of labels.
//!
//! A symbol set which comes with a table (see [`crate::label_set`]) is not
//! learned symbol by symbol: a building is learned as a grey fill and a black
//! outline, a road two meters wide as a strip of paved area with a black
//! border either side, and symbols which look alike as one class. So what a
//! network answers is parts, and a map written straight from them is a map
//! of parts: a `521.3` fill with a loose `415` line round it where the
//! picture had a `521` building, a thin polygon of paved area and two step
//! lines where it had a road. Every pixel of ink is in the right place, and
//! almost none of the objects are the ones a mapper would have drawn.
//!
//! This is the table run backwards. It works on the grids — the ground, the
//! lines layer and the points — before any of them become geometry, since
//! that is where the evidence is: whether a fill's boundary is covered by an
//! outline, how wide a strip is, how far apart two borders run. Nothing in
//! it knows ISSprOM; every rule comes out of the table and the symbols it
//! names, as [`Rules::of`] reads them.
//!
//! # The rules
//!
//! In the order [`assemble`] applies them, each on what the one before left:
//!
//! 1. **A strip with borders is a line.** A symbol labelled as `area` and
//!    `borders` — a road, a track — is looked for where its area class is
//!    narrow: the class's cells less their morphological opening by a disk
//!    a little wider than the widest such symbol, which is what takes the
//!    roads off a paved square and leaves the square. A narrow piece long
//!    enough to have a centre line, with its border class along at least
//!    [`Assemble::outline_cover`] of its sides, is thinned to that line, and
//!    each path of it is written as a line object of the symbol whose stroke
//!    is nearest the path's width — its cells over its length. The strip's
//!    cells are given back to the ground beside them, which is what is under
//!    a road, and the border pixels along it are spent.
//! 2. **A line with borders is chosen by their spacing.** A symbol labelled
//!    as `line` and `borders` — a stairway — is told from the others of its
//!    line class by how far the border pixels near each path lie from it:
//!    the median of that, against each symbol's main line half width and
//!    border shift.
//! 3. **A fill with an outline is one area.** A region of an area class
//!    which some symbol is labelled with as `area` and `outline` becomes an
//!    object of that symbol where the outline class covers at least
//!    [`Assemble::outline_cover`] of its boundary, and the outline pixels
//!    along it are spent. Where the class's own symbol is one the table says
//!    is never drawn alone — a building's fill — the region becomes the
//!    compound whatever the outline says, since there is nothing else it
//!    could be.
//! 4. **A point picks the variant of the line it sits on.** Where a point
//!    class labels several symbols — ISSprOM's slope lines of a contour, an
//!    index contour and a form line — each is taken to belong to the line
//!    symbol its number is a variant of (`102.1` of `102`, by the numbering
//!    both standards use), and a point becomes the one whose line runs
//!    nearest it.
//!
//! # What is left over
//!
//! A part which none of the rules takes stays what it was, written with its
//! class's own symbol: an outline with no fill beside it, borders with no
//! strip between them, a narrow strip with no borders. Each of those symbols
//! is one the standard draws on its own, so a leftover is a reading of the
//! picture and not a mistake — a step line is a step line, and a narrow
//! paved area is one. [`Report::left`] counts them, so that how much of an
//! answer did not come together is a number and not a guess.

use std::collections::{BTreeMap, HashMap};

use crate::label_set::{stroke_width, LabelSet, Part};
use crate::line_truth::MIN_WIDTH_PIXELS;
use crate::line_vector::{line_object, polylines, LineGrid, LineSettings, Polyline};
use crate::map::{Map, Object, Symbol};
use crate::point_vector::Peak;
use crate::symbol_kinds::{Catalogue, Entry, SymbolKind};
use crate::vectorize::{Placement, SymbolGrid};

/// How much of a fill's boundary, or of a strip's sides, its outline or
/// borders have to cover for the two to be taken as one symbol.
pub const DEFAULT_OUTLINE_COVER: f64 = 0.6;

/// What a cell of [`Assembled::chosen`] holds where it is written with its
/// class's own symbol.
pub const UNCHOSEN: u16 = u16::MAX;

/// Half the least width a line is labelled, in pixels: how far a line's band
/// reaches either side of where it runs, however thin the line.
const BAND: f64 = MIN_WIDTH_PIXELS as f64 / 2.0;

/// How much further than the geometry says a part is looked for, in pixels:
/// a network's bands wander by about that much.
const SLACK: f64 = 1.0;

/// How much wider than the widest strip symbol the opening which tells a
/// strip from an area is, as a share of that width.
const NARROW: f64 = 1.25;

/// No source: the nearest of nothing.
const NONE: u32 = u32::MAX;

/// The one choice [`assemble`] does not make for itself.
#[derive(Clone, Copy, Debug)]
pub struct Assemble {
    /// The share of a fill's boundary its outline has to cover, and of a
    /// strip's sides its borders have to, for them to be one symbol: from 0
    /// to 1.
    pub outline_cover: f64,
}

impl Default for Assemble {
    fn default() -> Assemble {
        Assemble {
            outline_cover: DEFAULT_OUTLINE_COVER,
        }
    }
}

/// A fill and the outline round it, as one area symbol.
#[derive(Clone, Debug)]
struct OutlineRule {
    area: usize,
    outline: usize,
    symbol: Entry,
    /// The outline's stroke, in mm.
    width: f64,
}

/// A strip of ground and the borders along it, as one line symbol.
#[derive(Clone, Debug)]
struct StripRule {
    area: usize,
    borders: usize,
    symbol: Entry,
    /// The strip's width, in mm.
    width: f64,
    /// How far past the strip's edge its border reaches, in mm.
    reach: f64,
}

/// A line and the borders along it, as one line symbol.
#[derive(Clone, Debug)]
struct BorderedRule {
    line: usize,
    borders: usize,
    symbol: Entry,
    /// How far from the line its borders run, middle to middle, in mm.
    offset: f64,
    /// How wide a border is, in mm.
    border_width: f64,
}

/// One symbol a point class may be, and the line classes it sits on.
#[derive(Clone, Debug)]
struct PointChoice {
    symbol: Entry,
    parents: Vec<usize>,
}

/// What a table says can be put back together: every symbol of several
/// parts, and every class which labels several symbols in a way the grids
/// can tell apart.
#[derive(Clone, Debug)]
pub struct Rules {
    outlines: Vec<OutlineRule>,
    strips: Vec<StripRule>,
    bordered: Vec<BorderedRule>,
    /// A line class's symbol labelled with it and nothing else, where its
    /// own symbol is not.
    plain: HashMap<usize, Entry>,
    /// Whether each area class's own symbol is drawn on its own.
    alone: Vec<bool>,
    /// For each point class, the symbols it may be; none where it is one.
    points: Vec<Vec<PointChoice>>,
    /// Whether each line class is some symbol's outline or borders, which is
    /// what makes what is left of it a leftover.
    absorbing: Vec<bool>,
}

impl Rules {
    /// The rules `labels` gives the symbol set `map`: none for a set with no
    /// table, whose every class is one symbol already.
    pub fn of(map: &Map, labels: &LabelSet) -> Rules {
        let catalogue = Catalogue::of(map);
        let mut rules = Rules {
            outlines: Vec::new(),
            strips: Vec::new(),
            bordered: Vec::new(),
            plain: HashMap::new(),
            alone: labels
                .areas
                .iter()
                .map(|entry| labels.drawn(entry.index))
                .collect(),
            points: vec![Vec::new(); labels.points.len()],
            absorbing: vec![false; labels.lines.len()],
        };
        // The line classes a symbol's number belongs to, for the points.
        let mut lines_of_code: HashMap<String, Vec<usize>> = HashMap::new();
        let mut point_symbols: Vec<Vec<Entry>> = vec![Vec::new(); labels.points.len()];

        for (kind, list) in catalogue.by_kind() {
            for entry in list {
                let parts = labels.parts(entry.index);
                let area = parts.iter().find_map(|p| match p {
                    &Part::Area(class) => Some(class),
                    _ => None,
                });
                let outline = parts.iter().find_map(|p| match p {
                    &Part::Outline(class) => Some(class),
                    _ => None,
                });
                let line = parts.iter().find_map(|p| match p {
                    &Part::Line(class) => Some(class),
                    _ => None,
                });
                let borders = parts.iter().find_map(|p| match p {
                    &Part::Borders(class) => Some(class),
                    _ => None,
                });
                if let Some(class) = line {
                    lines_of_code
                        .entry(entry.code.trim().to_string())
                        .or_default()
                        .push(class);
                }
                match kind {
                    SymbolKind::OpaqueArea => {
                        if let (Some(area), Some(outline)) = (area, outline) {
                            rules.absorbing[outline] = true;
                            rules.outlines.push(OutlineRule {
                                area,
                                outline,
                                symbol: entry.clone(),
                                width: stroke_width(map, labels.lines[outline].index),
                            });
                        }
                    }
                    SymbolKind::Line => {
                        let Some(Symbol::Line(symbol)) = map.symbols.get(entry.index) else {
                            continue;
                        };
                        let border = [&symbol.border, &symbol.right_border]
                            .into_iter()
                            .find(|border| border.is_visible());
                        match (area, line, borders, border) {
                            (Some(area), None, Some(borders), Some(border)) => {
                                rules.absorbing[borders] = true;
                                rules.strips.push(StripRule {
                                    area,
                                    borders,
                                    symbol: entry.clone(),
                                    width: symbol.line_width,
                                    reach: border.shift + border.width / 2.0,
                                });
                            }
                            (None, Some(line), Some(borders), Some(border)) => {
                                rules.absorbing[borders] = true;
                                rules.bordered.push(BorderedRule {
                                    line,
                                    borders,
                                    symbol: entry.clone(),
                                    offset: symbol.line_width / 2.0 + border.shift,
                                    border_width: border.width,
                                });
                            }
                            (None, Some(line), None, _) if parts.len() == 1 => {
                                rules.plain.entry(line).or_insert_with(|| entry.clone());
                            }
                            _ => {}
                        }
                    }
                    SymbolKind::Point => {
                        if let [Part::Point(class)] = parts {
                            point_symbols[*class].push(entry.clone());
                        }
                    }
                    _ => {}
                }
            }
        }

        // A point class of several symbols, each the variant of a line.
        for (class, symbols) in point_symbols.into_iter().enumerate() {
            if symbols.len() < 2 {
                continue;
            }
            let choices: Vec<PointChoice> = symbols
                .into_iter()
                .map(|symbol| {
                    let code = symbol.code.trim();
                    let parents = code
                        .rsplit_once('.')
                        .and_then(|(parent, _)| lines_of_code.get(parent))
                        .cloned()
                        .unwrap_or_default();
                    PointChoice { symbol, parents }
                })
                .collect();
            if choices.iter().any(|choice| !choice.parents.is_empty()) {
                rules.points[class] = choices;
            }
        }
        rules
    }

    /// Whether there is anything to put together at all.
    pub fn is_empty(&self) -> bool {
        self.outlines.is_empty()
            && self.strips.is_empty()
            && self.bordered.is_empty()
            && self.points.iter().all(Vec::is_empty)
            && self.alone.iter().all(|&alone| alone)
    }

    /// The symbol each area class is written with where there is nothing to
    /// go on but the class: its own, unless that is never drawn alone, and
    /// then the symbol it is the fill of. What a network with no lines
    /// layer — a tree of curves — has its areas written with.
    pub fn area_symbols(&self, labels: &LabelSet) -> Vec<Entry> {
        labels
            .areas
            .iter()
            .enumerate()
            .map(|(class, own)| {
                if self.alone[class] {
                    return own.clone();
                }
                self.outlines
                    .iter()
                    .find(|rule| rule.area == class)
                    .map(|rule| rule.symbol.clone())
                    .unwrap_or_else(|| own.clone())
            })
            .collect()
    }
}

/// What kind of part was left as it was.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LeftoverKind {
    /// A line of an outline or border class which no fill or strip took.
    Line,
    /// A narrow strip of an area class with no borders along it.
    Strip,
}

/// How many parts of one class were left as they were.
#[derive(Clone, Debug, PartialEq)]
pub struct Leftover {
    /// What kind of part.
    pub kind: LeftoverKind,
    /// The class's name.
    pub class: String,
    /// The number of the symbol they were written with: the class's own.
    pub symbol: String,
    /// How many.
    pub count: usize,
}

/// What [`assemble`] put together, and what it left.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// How many regions became an area symbol other than their class's own.
    pub areas: usize,
    /// How many strips became line objects.
    pub strips: usize,
    /// How many lines were given the symbol their borders' spacing says.
    pub bordered: usize,
    /// How many points became another symbol than their class's own.
    pub points: usize,
    /// What was left as it was, class by class.
    pub left: Vec<Leftover>,
}

impl Report {
    /// Counts the lines of `traced` — the line objects of what was left of
    /// the lines layer — which are of an outline or border class: lines no
    /// fill or strip took.
    pub fn count_left_lines(&mut self, traced: &[Object], labels: &LabelSet, rules: &Rules) {
        for (class, entry) in labels.lines.iter().enumerate() {
            if !rules.absorbing[class] {
                continue;
            }
            let count = traced
                .iter()
                .filter(|object| object.symbol_index == Some(entry.index))
                .count();
            if count > 0 {
                self.left.push(Leftover {
                    kind: LeftoverKind::Line,
                    class: entry.name.clone(),
                    symbol: entry.code.trim().to_string(),
                    count,
                });
            }
        }
    }
}

/// The grids, put together.
pub struct Assembled {
    /// The ground, with the strips which became lines given back to the
    /// ground beside them.
    pub grid: SymbolGrid,
    /// The lines layer, less what was spent and less the line classes whose
    /// paths are among [`Assembled::objects`] already.
    pub lines: Option<LineGrid>,
    /// The symbols a cell of [`Assembled::chosen`] may name.
    pub areas: Vec<Entry>,
    /// For each cell, the place in [`Assembled::areas`] of the symbol its
    /// region is written with, or [`UNCHOSEN`] for its class's own.
    pub chosen: Vec<u16>,
    /// The line objects made here: the strips and the bordered lines.
    pub objects: Vec<Object>,
    /// For each peak, the symbol it is written with where that is not its
    /// class's own.
    pub point_symbols: Vec<Option<Entry>>,
    /// What came together and what did not.
    pub report: Report,
}

/// Puts a network's answer for a picture back together, by `rules` — see
/// the module documentation. `labels` are the classes the answer is in,
/// `placement` the ground the grids cover (which is what says how many
/// pixels a millimeter of paper is), and `line_settings` how the lines layer
/// is traced, which the strips and the bordered lines are traced by too.
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    grid: &SymbolGrid,
    lines: Option<&LineGrid>,
    points: &[Peak],
    labels: &LabelSet,
    rules: &Rules,
    placement: &Placement,
    line_settings: &LineSettings,
    settings: &Assemble,
) -> Result<Assembled, String> {
    if !(0.0..=1.0).contains(&settings.outline_cover) {
        return Err(format!(
            "the share of a boundary an outline covers is from 0 to 1, and is {}",
            settings.outline_cover
        ));
    }
    if let Some(lines) = lines {
        if (lines.width, lines.height) != (grid.width, grid.height) {
            return Err(format!(
                "the lines layer is {}x{} pixels and the ground {}x{}",
                lines.width, lines.height, grid.width, grid.height
            ));
        }
    }
    // How many mm of paper a cell is.
    let cell_mm =
        placement.ground.width() / grid.width as f64 * 1000.0 / placement.scale_denominator as f64;
    let mut work = Work {
        width: grid.width,
        height: grid.height,
        px: 1.0 / cell_mm,
        grid: grid.clone(),
        lines: lines.cloned(),
        objects: Vec::new(),
        report: Report::default(),
        placement,
        line_settings,
        cover: settings.outline_cover,
    };
    work.strips(rules, labels);
    work.bordered(rules, labels);
    let (areas, chosen) = work.areas(rules);
    let point_symbols = work.points(rules, labels, points);
    Ok(Assembled {
        grid: work.grid,
        lines: work.lines,
        areas,
        chosen,
        objects: work.objects,
        point_symbols,
        report: work.report,
    })
}

/// The grids on their way through the rules.
struct Work<'a> {
    width: usize,
    height: usize,
    /// How many pixels a mm of paper is.
    px: f64,
    grid: SymbolGrid,
    lines: Option<LineGrid>,
    objects: Vec<Object>,
    report: Report,
    placement: &'a Placement,
    line_settings: &'a LineSettings,
    cover: f64,
}

impl Work<'_> {
    /// Where the line class `class` runs, as the nearest of its pixels to
    /// every pixel; nothing anywhere where there is no lines layer.
    fn near_line(&self, class: usize) -> Nearest {
        let sources: Vec<bool> = match &self.lines {
            Some(lines) => lines.channel(class).iter().map(|&on| on != 0).collect(),
            None => vec![false; self.width * self.height],
        };
        nearest(&sources, self.width, self.height)
    }

    /// Turns the pixels of line class `class` off wherever `spent` says.
    fn spend(&mut self, class: usize, spent: impl Fn(usize) -> bool) {
        if let Some(lines) = &mut self.lines {
            for (at, on) in lines.channel_mut(class).iter_mut().enumerate() {
                if *on != 0 && spent(at) {
                    *on = 0;
                }
            }
        }
    }

    /// The four neighbours of a cell which are in the picture.
    fn around(&self, at: usize) -> impl Iterator<Item = usize> {
        let (width, height) = (self.width, self.height);
        let (x, y) = (at % width, at / width);
        [
            (x > 0).then(|| at - 1),
            (x + 1 < width).then(|| at + 1),
            (y > 0).then(|| at - width),
            (y + 1 < height).then(|| at + width),
        ]
        .into_iter()
        .flatten()
    }

    /// The share of the sides of `cells` against cells which are not
    /// `inside` along which a pixel of `near` lies within `reach` pixels of
    /// one cell or the other. The picture's edge is not a side: what is past
    /// it is not known. `None` for cells with no side at all.
    fn coverage(
        &self,
        cells: &[usize],
        inside: &[bool],
        near: &Nearest,
        reach: f64,
    ) -> Option<f64> {
        let reach = (reach * reach) as f32;
        let (mut sides, mut covered) = (0usize, 0usize);
        for &at in cells {
            for next in self.around(at) {
                if inside[next] {
                    continue;
                }
                sides += 1;
                if near.dist2[at] <= reach || near.dist2[next] <= reach {
                    covered += 1;
                }
            }
        }
        (sides > 0).then(|| covered as f64 / sides as f64)
    }

    /// Rule 1: the strips with borders, as lines.
    fn strips(&mut self, rules: &Rules, labels: &LabelSet) {
        let mut by_area: BTreeMap<usize, Vec<&StripRule>> = BTreeMap::new();
        for rule in &rules.strips {
            by_area.entry(rule.area).or_default().push(rule);
        }
        let (width, height, px) = (self.width, self.height, self.px);
        for (area, strips) in by_area {
            let inside: Vec<bool> = self.grid.class.iter().map(|&c| c == area as u16).collect();
            if !inside.contains(&true) {
                continue;
            }
            // The opening: a disk a little wider than the widest strip, and
            // what of the class it cannot reach is narrow.
            let widest = strips.iter().map(|rule| rule.width).fold(0.0, f64::max) * self.px;
            let radius = NARROW * widest / 2.0 + 0.5;
            let outside: Vec<bool> = inside.iter().map(|&on| !on).collect();
            let out = nearest(&outside, width, height);
            let core: Vec<bool> = (0..width * height)
                .map(|at| inside[at] && (out.dist2[at] as f64).sqrt() - 0.5 > radius)
                .collect();
            let to_core = nearest(&core, width, height);
            let reach = ((radius + 0.5) * (radius + 0.5)) as f32;
            let narrow: Vec<bool> = (0..width * height)
                .map(|at| inside[at] && to_core.dist2[at] > reach)
                .collect();

            let mut border_classes: Vec<usize> = strips.iter().map(|rule| rule.borders).collect();
            border_classes.sort_unstable();
            border_classes.dedup();
            let near: HashMap<usize, Nearest> = border_classes
                .iter()
                .map(|&class| (class, self.near_line(class)))
                .collect();
            let reach_of = |class: usize| {
                strips
                    .iter()
                    .filter(|rule| rule.borders == class)
                    .map(|rule| rule.reach * px)
                    .fold(0.0, f64::max)
                    + BAND
                    + SLACK
            };

            let settings = LineSettings {
                min_length: self.line_settings.min_length.max(2.0 * radius),
                ..*self.line_settings
            };
            let mut spent: HashMap<usize, Vec<bool>> = HashMap::new();
            let mut left = 0;
            for cells in components(&narrow, width, height) {
                let window = Window::around(&cells, width, height);
                let paths = window.polylines(&cells, &settings);
                if paths.is_empty() {
                    continue;
                }
                let best = border_classes
                    .iter()
                    .filter_map(|&class| {
                        self.coverage(&cells, &inside, &near[&class], reach_of(class))
                            .map(|share| (class, share))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1));
                let Some((borders, _)) = best.filter(|&(_, share)| share >= self.cover) else {
                    left += 1;
                    continue;
                };

                // Each cell of the strip to the path nearest it, so that a
                // path's width is its cells over its length.
                let owners = window.owners(&cells, &paths);
                let mut count = vec![0usize; paths.len()];
                for owner in owners.into_iter().flatten() {
                    count[owner] += 1;
                }
                for (path, cells_of) in paths.iter().zip(count) {
                    let length = length_of(path);
                    if length <= 0.0 {
                        continue;
                    }
                    let measured = cells_of as f64 / length / self.px;
                    let Some(rule) =
                        strips
                            .iter()
                            .filter(|rule| rule.borders == borders)
                            .min_by(|a, b| {
                                (a.width - measured)
                                    .abs()
                                    .total_cmp(&(b.width - measured).abs())
                            })
                    else {
                        continue;
                    };
                    let path = self.reach_ends(path, rule.width * px + 2.0, &inside, &narrow);
                    self.objects.push(line_object(
                        &path.points,
                        path.closed,
                        &rule.symbol,
                        self.placement,
                        width,
                        height,
                    ));
                    self.report.strips += 1;
                }

                // What is under a road is the ground beside it.
                for &at in &cells {
                    let from = out.source[at];
                    if from != NONE {
                        self.grid.class[at] = self.grid.class[from as usize];
                        self.grid.rotation[at] = self.grid.rotation[from as usize];
                    }
                }
                let mask = spent
                    .entry(borders)
                    .or_insert_with(|| vec![false; width * height]);
                for &at in &cells {
                    mask[at] = true;
                }
            }
            for (class, mask) in spent {
                let along = nearest(&mask, width, height);
                let reach = reach_of(class);
                let reach = (reach * reach) as f32;
                self.spend(class, |at| along.dist2[at] <= reach);
            }
            if left > 0 {
                let entry = &labels.areas[area];
                self.report.left.push(Leftover {
                    kind: LeftoverKind::Strip,
                    class: entry.name.clone(),
                    symbol: entry.code.trim().to_string(),
                    count: left,
                });
            }
        }
    }

    /// A strip's centre line stops about half its width short of where the
    /// strip does. Where the strip runs on into an area of its own class or
    /// off the picture, the end is walked on along the strip until it gets
    /// there — no further than `reach` pixels — so the road reaches the
    /// square it leads into, and the edge of the map it runs off. Where it
    /// ends in other ground it is left be: that is where the road ends.
    fn reach_ends(
        &self,
        path: &Polyline,
        reach: f64,
        inside: &[bool],
        narrow: &[bool],
    ) -> Polyline {
        let mut path = path.clone();
        if path.closed || path.points.len() < 2 {
            return path;
        }
        let last = path.points.len() - 1;
        for (end, before) in [(0, 1), (last, last - 1)] {
            let (x, y) = path.points[end];
            let (bx, by) = path.points[before];
            let length = (x - bx).hypot(y - by);
            if length <= 0.0 {
                continue;
            }
            let (dx, dy) = ((x - bx) / length, (y - by) / length);
            let mut step = 0.5;
            while step <= reach {
                let (px, py) = (x + dx * step, y + dy * step);
                if px < 0.0 || py < 0.0 || px >= self.width as f64 || py >= self.height as f64 {
                    let to_edge = [
                        (dx < 0.0).then(|| -x / dx),
                        (dx > 0.0).then(|| (self.width as f64 - x) / dx),
                        (dy < 0.0).then(|| -y / dy),
                        (dy > 0.0).then(|| (self.height as f64 - y) / dy),
                    ]
                    .into_iter()
                    .flatten()
                    .fold(f64::MAX, f64::min);
                    path.points[end] = (x + dx * to_edge, y + dy * to_edge);
                    break;
                }
                let at = py as usize * self.width + px as usize;
                if !inside[at] {
                    break;
                }
                if !narrow[at] {
                    path.points[end] = (px, py);
                    break;
                }
                step += 0.5;
            }
        }
        path
    }

    /// Rule 2: the lines with borders, by their spacing.
    fn bordered(&mut self, rules: &Rules, labels: &LabelSet) {
        let mut by_line: BTreeMap<usize, Vec<&BorderedRule>> = BTreeMap::new();
        for rule in &rules.bordered {
            by_line.entry(rule.line).or_default().push(rule);
        }
        let (width, height) = (self.width, self.height);
        for (line, bordered) in by_line {
            let Some(lines) = &self.lines else {
                return;
            };
            let paths = polylines(lines.channel(line), width, height, self.line_settings);
            if paths.is_empty() {
                continue;
            }
            // Every pixel to the path nearest it.
            let mut path_at = vec![NONE; width * height];
            for (id, path) in paths.iter().enumerate() {
                for at in pixels_of(path, width, height) {
                    path_at[at] = id as u32;
                }
            }
            let sources: Vec<bool> = path_at.iter().map(|&id| id != NONE).collect();
            let along = nearest(&sources, width, height);
            let reach = bordered
                .iter()
                .map(|rule| (rule.offset + rule.border_width / 2.0) * self.px)
                .fold(0.0, f64::max)
                + BAND
                + SLACK;
            let reach2 = (reach * reach) as f32;
            let owner = |at: usize| -> Option<usize> {
                (along.dist2[at] <= reach2 && along.source[at] != NONE)
                    .then(|| path_at[along.source[at] as usize] as usize)
            };

            let mut border_classes: Vec<usize> = bordered.iter().map(|rule| rule.borders).collect();
            border_classes.sort_unstable();
            border_classes.dedup();
            // How far each border pixel near a path lies from it.
            let mut spacing: HashMap<(usize, usize), Vec<f32>> = HashMap::new();
            for &class in &border_classes {
                for (at, &on) in lines.channel(class).iter().enumerate() {
                    if on == 0 {
                        continue;
                    }
                    if let Some(path) = owner(at) {
                        spacing
                            .entry((path, class))
                            .or_default()
                            .push(along.dist2[at].sqrt());
                    }
                }
            }

            let mut chosen: Vec<Option<usize>> = vec![None; paths.len()];
            for (id, path) in paths.iter().enumerate() {
                let best = bordered
                    .iter()
                    .filter_map(|rule| {
                        let found = spacing.get_mut(&(id, rule.borders))?;
                        let median = median(found)?;
                        Some((rule, (median - rule.offset * self.px).abs()))
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                let symbol = match best {
                    Some((rule, _)) => {
                        chosen[id] = Some(rule.borders);
                        self.report.bordered += 1;
                        &rule.symbol
                    }
                    // No borders to go on: the symbol which draws none, where
                    // the class has one.
                    None => rules.plain.get(&line).unwrap_or(&labels.lines[line]),
                };
                self.objects.push(line_object(
                    &path.points,
                    path.closed,
                    symbol,
                    self.placement,
                    width,
                    height,
                ));
            }
            // Every path of the class is written: none is traced again.
            self.spend(line, |_| true);
            for &class in &border_classes {
                self.spend(class, |at| {
                    owner(at).is_some_and(|path| chosen[path] == Some(class))
                });
            }
        }
    }

    /// Rule 3: the fills with outlines, as one area. The symbols a cell may
    /// be written with, and which of them each is.
    fn areas(&mut self, rules: &Rules) -> (Vec<Entry>, Vec<u16>) {
        let (width, height) = (self.width, self.height);
        let mut by_area: BTreeMap<usize, Vec<(usize, &OutlineRule)>> = BTreeMap::new();
        for (place, rule) in rules.outlines.iter().enumerate() {
            by_area.entry(rule.area).or_default().push((place, rule));
        }
        let areas: Vec<Entry> = rules
            .outlines
            .iter()
            .map(|rule| rule.symbol.clone())
            .collect();
        let mut chosen = vec![UNCHOSEN; width * height];
        if by_area.is_empty() {
            return (areas, chosen);
        }

        let mut near: HashMap<usize, Nearest> = HashMap::new();
        for rule in &rules.outlines {
            near.entry(rule.outline)
                .or_insert_with(|| self.near_line(rule.outline));
        }
        let px = self.px;
        let reach_of = |rule: &OutlineRule| (rule.width * px / 2.0).max(BAND) + SLACK;
        let mut spent: HashMap<usize, (Vec<bool>, f64)> = HashMap::new();

        for (area, outlined) in by_area {
            let inside: Vec<bool> = self.grid.class.iter().map(|&c| c == area as u16).collect();
            for cells in components(&inside, width, height) {
                let best = outlined
                    .iter()
                    .map(|&(place, rule)| {
                        let share = self
                            .coverage(&cells, &inside, &near[&rule.outline], reach_of(rule))
                            .unwrap_or(0.0);
                        (place, rule, share)
                    })
                    .max_by(|a, b| a.2.total_cmp(&b.2));
                let Some((place, rule, share)) = best else {
                    continue;
                };
                if share < self.cover && rules.alone[area] {
                    continue;
                }
                for &at in &cells {
                    chosen[at] = place as u16;
                }
                self.report.areas += 1;
                // The outline is spent along both sides of the boundary.
                let (mask, reach) = spent
                    .entry(rule.outline)
                    .or_insert_with(|| (vec![false; width * height], 0.0));
                *reach = reach.max(reach_of(rule));
                for &at in &cells {
                    let edge = self.around(at).any(|next| !inside[next]);
                    if edge {
                        mask[at] = true;
                        for next in self.around(at) {
                            if !inside[next] {
                                mask[next] = true;
                            }
                        }
                    }
                }
            }
        }
        for (class, (mask, reach)) in spent {
            let along = nearest(&mask, width, height);
            let reach = (reach * reach) as f32;
            self.spend(class, |at| along.dist2[at] <= reach);
        }
        (areas, chosen)
    }

    /// Rule 4: each point the variant of the line it sits on.
    fn points(&mut self, rules: &Rules, labels: &LabelSet, points: &[Peak]) -> Vec<Option<Entry>> {
        let mut near: HashMap<usize, Nearest> = HashMap::new();
        let reach = BAND + SLACK + 0.25 * self.px;
        let reach2 = (reach * reach) as f32;
        let mut found = Vec::with_capacity(points.len());
        for peak in points {
            let Some(choices) = rules.points.get(peak.class).filter(|c| !c.is_empty()) else {
                found.push(None);
                continue;
            };
            let (x, y) = (peak.x.floor(), peak.y.floor());
            if x < 0.0 || y < 0.0 || x >= self.width as f64 || y >= self.height as f64 {
                found.push(None);
                continue;
            }
            let at = y as usize * self.width + x as usize;
            let mut best: Option<(&Entry, f32)> = None;
            for choice in choices {
                for &parent in &choice.parents {
                    if let std::collections::hash_map::Entry::Vacant(slot) = near.entry(parent) {
                        slot.insert(self.near_line(parent));
                    }
                    let distance = near[&parent].dist2[at];
                    if distance <= reach2 && best.is_none_or(|(_, d)| distance < d) {
                        best = Some((&choice.symbol, distance));
                    }
                }
            }
            let symbol = best
                .map(|(symbol, _)| symbol)
                .filter(|symbol| symbol.index != labels.points[peak.class].index)
                .cloned();
            if symbol.is_some() {
                self.report.points += 1;
            }
            found.push(symbol);
        }
        found
    }
}

/// The smallest rectangle of a picture which holds some cells, and a pixel
/// more all round where the picture has one: what a component is thinned
/// and measured in, rather than the whole picture once a component.
struct Window {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    /// How wide the whole picture is.
    stride: usize,
}

impl Window {
    fn around(cells: &[usize], width: usize, height: usize) -> Window {
        let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
        for &at in cells {
            let (x, y) = (at % width, at / width);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
        let (left, top) = (left.saturating_sub(1), top.saturating_sub(1));
        let (right, bottom) = ((right + 1).min(width - 1), (bottom + 1).min(height - 1));
        Window {
            left,
            top,
            width: right - left + 1,
            height: bottom - top + 1,
            stride: width,
        }
    }

    /// Where a cell of the picture is in the window.
    fn local(&self, at: usize) -> usize {
        let (x, y) = (at % self.stride, at / self.stride);
        (y - self.top) * self.width + (x - self.left)
    }

    /// The centre lines of `cells`, in the picture's own pixels.
    fn polylines(&self, cells: &[usize], settings: &LineSettings) -> Vec<Polyline> {
        let mut mask = vec![0u8; self.width * self.height];
        for &at in cells {
            mask[self.local(at)] = 1;
        }
        let mut paths = polylines(&mask, self.width, self.height, settings);
        for path in &mut paths {
            for point in &mut path.points {
                point.0 += self.left as f64;
                point.1 += self.top as f64;
            }
        }
        paths
    }

    /// The path of `paths` nearest each of `cells`.
    fn owners(&self, cells: &[usize], paths: &[Polyline]) -> Vec<Option<usize>> {
        let mut path_at = vec![NONE; self.width * self.height];
        for (id, path) in paths.iter().enumerate() {
            let local = Polyline {
                points: path
                    .points
                    .iter()
                    .map(|&(x, y)| (x - self.left as f64, y - self.top as f64))
                    .collect(),
                closed: path.closed,
            };
            for at in pixels_of(&local, self.width, self.height) {
                path_at[at] = id as u32;
            }
        }
        let sources: Vec<bool> = path_at.iter().map(|&id| id != NONE).collect();
        let along = nearest(&sources, self.width, self.height);
        cells
            .iter()
            .map(|&at| {
                let source = along.source[self.local(at)];
                (source != NONE).then(|| path_at[source as usize] as usize)
            })
            .collect()
    }
}

/// How long a path is, in pixels.
fn length_of(path: &Polyline) -> f64 {
    let mut length: f64 = path
        .points
        .windows(2)
        .map(|pair| (pair[0].0 - pair[1].0).hypot(pair[0].1 - pair[1].1))
        .sum();
    if path.closed {
        if let (Some(first), Some(last)) = (path.points.first(), path.points.last()) {
            length += (first.0 - last.0).hypot(first.1 - last.1);
        }
    }
    length
}

/// The pixels a path runs through, sampled finely enough to miss none.
fn pixels_of(path: &Polyline, width: usize, height: usize) -> Vec<usize> {
    let mut points = path.points.clone();
    if path.closed {
        if let Some(&first) = points.first() {
            points.push(first);
        }
    }
    let mut found = Vec::new();
    let mut put = |(x, y): (f64, f64)| {
        if x >= 0.0 && y >= 0.0 && x < width as f64 && y < height as f64 {
            found.push(y as usize * width + x as usize);
        }
    };
    if points.len() == 1 {
        put(points[0]);
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let steps = ((a.0 - b.0).hypot(a.1 - b.1) * 2.0).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            put((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// The middle value of some, which it sorts.
fn median(values: &mut [f32]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f32::total_cmp);
    Some(values[values.len() / 2] as f64)
}

/// The 4-connected runs of `mask`, each its cells in reading order.
fn components(mask: &[bool], width: usize, height: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; mask.len()];
    let mut found = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut cells = Vec::new();
        seen[start] = true;
        stack.push(start);
        while let Some(at) = stack.pop() {
            cells.push(at);
            let (x, y) = (at % width, at / width);
            let next = [
                (x > 0).then(|| at - 1),
                (x + 1 < width).then(|| at + 1),
                (y > 0).then(|| at - width),
                (y + 1 < height).then(|| at + width),
            ];
            for next in next.into_iter().flatten() {
                if mask[next] && !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        cells.sort_unstable();
        found.push(cells);
    }
    found
}

/// The nearest source pixel to every pixel of a picture.
struct Nearest {
    /// The squared distance to it, from middle to middle, in pixels; infinite
    /// where there is no source.
    dist2: Vec<f32>,
    /// Which pixel it is, or [`NONE`].
    source: Vec<u32>,
}

/// The exact Euclidean distance transform of `sources`, with the nearest
/// source of each pixel: Felzenszwalb and Huttenlocher's lower envelope of
/// parabolas, down the columns and then along the rows.
fn nearest(sources: &[bool], width: usize, height: usize) -> Nearest {
    let cells = width * height;
    // Down each column: the nearest source in it, by row.
    let mut column_d = vec![f64::INFINITY; cells];
    let mut column_row = vec![NONE; cells];
    for x in 0..width {
        let mut last: Option<usize> = None;
        for y in 0..height {
            if sources[y * width + x] {
                last = Some(y);
            }
            if let Some(row) = last {
                column_d[y * width + x] = ((y - row) * (y - row)) as f64;
                column_row[y * width + x] = row as u32;
            }
        }
        let mut last: Option<usize> = None;
        for y in (0..height).rev() {
            if sources[y * width + x] {
                last = Some(y);
            }
            if let Some(row) = last {
                let d = ((row - y) * (row - y)) as f64;
                if d < column_d[y * width + x] {
                    column_d[y * width + x] = d;
                    column_row[y * width + x] = row as u32;
                }
            }
        }
    }

    let mut dist2 = vec![f32::INFINITY; cells];
    let mut source = vec![NONE; cells];
    let mut hull: Vec<usize> = Vec::with_capacity(width);
    let mut starts: Vec<f64> = Vec::with_capacity(width);
    for y in 0..height {
        let f = |x: usize| column_d[y * width + x];
        hull.clear();
        starts.clear();
        for q in 0..width {
            if !f(q).is_finite() {
                continue;
            }
            loop {
                let Some(&p) = hull.last() else {
                    hull.push(q);
                    starts.push(f64::NEG_INFINITY);
                    break;
                };
                let (qf, pf) = (q as f64, p as f64);
                let s = ((f(q) + qf * qf) - (f(p) + pf * pf)) / (2.0 * (qf - pf));
                if s <= *starts.last().expect("as long as the hull") {
                    hull.pop();
                    starts.pop();
                    continue;
                }
                hull.push(q);
                starts.push(s);
                break;
            }
        }
        if hull.is_empty() {
            continue;
        }
        let mut k = 0;
        for x in 0..width {
            while k + 1 < hull.len() && starts[k + 1] < x as f64 {
                k += 1;
            }
            let p = hull[k];
            let dx = x as f64 - p as f64;
            dist2[y * width + x] = (dx * dx + f(p)) as f32;
            source[y * width + x] = column_row[y * width + p] * width as u32 + p as u32;
        }
    }
    Nearest { dist2, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::xml_reader::read_xml_map;
    use std::path::Path;

    const ALMOST: &str = "symbols_sets/almost_ISSprOM.omap";

    fn almost() -> (Map, LabelSet, Rules) {
        let (mut map, _) = read_xml_map(Path::new(ALMOST)).expect("the set");
        map.resolve_references();
        let labels = LabelSet::beside(&map, Path::new(ALMOST)).expect("its table");
        let rules = Rules::of(&map, &labels);
        (map, labels, rules)
    }

    fn area(labels: &LabelSet, name: &str) -> usize {
        labels
            .areas
            .iter()
            .position(|e| e.name == name)
            .expect(name)
    }

    fn line(labels: &LabelSet, name: &str) -> usize {
        labels
            .lines
            .iter()
            .position(|e| e.name == name)
            .expect(name)
    }

    fn point(labels: &LabelSet, name: &str) -> usize {
        labels
            .points
            .iter()
            .position(|e| e.name == name)
            .expect(name)
    }

    fn code(map: &Map, object: &Object) -> String {
        map.symbols[object.symbol_index.expect("a symbol")]
            .code()
            .trim()
            .to_string()
    }

    /// A picture of `size` pixels square at 3 px/m of ground, at the set's
    /// own 1:4000: 12 pixels to a mm of paper.
    fn placement(map: &Map, size: usize) -> Placement {
        let resolution = 3.0;
        let ground = size as f64 / resolution;
        Placement {
            ground: Rect::from_ltrb(-ground / 2.0, -ground / 2.0, ground / 2.0, ground / 2.0),
            scale_denominator: map.scale_denominator,
        }
    }

    fn fill(
        grid: &mut SymbolGrid,
        class: usize,
        x: std::ops::Range<usize>,
        y: std::ops::Range<usize>,
    ) {
        for row in y {
            for column in x.clone() {
                grid.class[row * grid.width + column] = class as u16;
            }
        }
    }

    /// Draws a ring of line pixels `band` wide round a rectangle of cells.
    fn ring(
        lines: &mut LineGrid,
        class: usize,
        x: std::ops::Range<usize>,
        y: std::ops::Range<usize>,
        band: usize,
    ) {
        let width = lines.width;
        let channel = lines.channel_mut(class);
        let half = band / 2;
        for row in y.start - half..y.end + half {
            for column in x.start - half..x.end + half {
                let inner = column >= x.start + band - half
                    && column < x.end - band + half
                    && row >= y.start + band - half
                    && row < y.end - band + half;
                if !inner {
                    channel[row * width + column] = 1;
                }
            }
        }
    }

    fn run(
        map: &Map,
        labels: &LabelSet,
        rules: &Rules,
        grid: &SymbolGrid,
        lines: &LineGrid,
        points: &[Peak],
    ) -> Assembled {
        assemble(
            grid,
            Some(lines),
            points,
            labels,
            rules,
            &placement(map, grid.width),
            &LineSettings::default(),
            &Assemble::default(),
        )
        .expect("assembled")
    }

    #[test]
    fn the_table_of_almost_issprom_comes_to_its_compound_symbols() {
        let (_, labels, rules) = almost();
        let codes = |list: Vec<&Entry>| {
            let mut codes: Vec<String> = list.iter().map(|e| e.code.trim().to_string()).collect();
            codes.sort();
            codes
        };
        assert_eq!(
            codes(rules.outlines.iter().map(|r| &r.symbol).collect()),
            ["301", "301.2", "302", "501", "501.10", "521", "522"]
        );
        assert_eq!(
            codes(rules.strips.iter().map(|r| &r.symbol).collect()),
            [
                "501.16", "501.17", "501.18", "501.19", "501.6", "501.7", "501.8", "501.9",
                "505.1", "505.2"
            ]
        );
        assert_eq!(
            codes(rules.bordered.iter().map(|r| &r.symbol).collect()),
            ["532.6", "532.7", "532.8", "532.9"]
        );
        let building = area(&labels, "Building");
        assert!(
            !rules.alone[building],
            "a building's fill is never drawn alone"
        );
        assert!(rules.alone[area(&labels, "Paved area, light traffic")]);
        assert_eq!(rules.area_symbols(&labels)[building].code.trim(), "521");
        let slope = &rules.points[point(&labels, "Slope line")];
        assert_eq!(
            codes(slope.iter().map(|c| &c.symbol).collect()),
            ["101.1", "102.1", "103.1"]
        );
        assert!(rules.absorbing[line(&labels, "Black line 0.14 mm")]);
        assert!(!rules.absorbing[line(&labels, "Contour")]);
    }

    #[test]
    fn a_set_with_no_table_has_nothing_to_put_together() {
        let (map, _, _) = almost();
        let labels = LabelSet::of(&map);
        assert!(Rules::of(&map, &labels).is_empty());
    }

    #[test]
    fn a_fill_never_drawn_alone_is_its_compound_with_or_without_an_outline() {
        let (map, labels, rules) = almost();
        let mut grid = SymbolGrid::new(60, 60);
        fill(&mut grid, area(&labels, "Forest"), 0..60, 0..60);
        fill(&mut grid, area(&labels, "Building"), 10..30, 10..30);
        let lines = LineGrid::new(60, 60, labels.lines.len());
        let done = run(&map, &labels, &rules, &grid, &lines, &[]);
        let at = 20 * 60 + 20;
        assert_ne!(done.chosen[at], UNCHOSEN);
        assert_eq!(done.areas[done.chosen[at] as usize].code.trim(), "521");
        assert_eq!(done.chosen[0], UNCHOSEN, "the forest is its own symbol");
        assert_eq!(done.report.areas, 1);
    }

    #[test]
    fn a_paved_area_with_its_outline_is_one_area_and_the_outline_is_spent() {
        let (map, labels, rules) = almost();
        let paved = area(&labels, "Paved area, light traffic");
        let black = line(&labels, "Black line 0.10 mm");
        let mut grid = SymbolGrid::new(80, 80);
        fill(&mut grid, area(&labels, "Open land"), 0..80, 0..80);
        fill(&mut grid, paved, 20..60, 20..60);
        let mut lines = LineGrid::new(80, 80, labels.lines.len());
        ring(&mut lines, black, 20..60, 20..60, 3);
        let done = run(&map, &labels, &rules, &grid, &lines, &[]);
        let at = 40 * 80 + 40;
        assert_eq!(done.areas[done.chosen[at] as usize].code.trim(), "501");
        let left = done.lines.expect("lines");
        assert!(
            !left.channel(black).contains(&1),
            "the outline went into the area"
        );
    }

    #[test]
    fn a_paved_area_with_no_outline_stays_a_paved_area() {
        let (map, labels, rules) = almost();
        let paved = area(&labels, "Paved area, light traffic");
        let mut grid = SymbolGrid::new(80, 80);
        fill(&mut grid, area(&labels, "Open land"), 0..80, 0..80);
        fill(&mut grid, paved, 20..60, 20..60);
        let lines = LineGrid::new(80, 80, labels.lines.len());
        let done = run(&map, &labels, &rules, &grid, &lines, &[]);
        assert_eq!(done.chosen[40 * 80 + 40], UNCHOSEN);
        assert_eq!(done.report.areas, 0);
    }

    #[test]
    fn an_outline_with_no_fill_is_left_as_its_own_line() {
        let (map, labels, rules) = almost();
        let black = line(&labels, "Black line 0.14 mm");
        let mut grid = SymbolGrid::new(80, 80);
        fill(&mut grid, area(&labels, "Open land"), 0..80, 0..80);
        let mut lines = LineGrid::new(80, 80, labels.lines.len());
        ring(&mut lines, black, 20..60, 20..60, 3);
        let done = run(&map, &labels, &rules, &grid, &lines, &[]);
        let left = done.lines.expect("lines");
        assert!(left.channel(black).contains(&1), "nothing took the line");
        let traced = crate::line_vector::objects_of_lines(
            &left,
            &labels.lines,
            &placement(&map, 80),
            &LineSettings::default(),
        )
        .expect("traced");
        let mut report = Report::default();
        report.count_left_lines(&traced, &labels, &rules);
        assert_eq!(report.left.len(), 1);
        assert_eq!(report.left[0].kind, LeftoverKind::Line);
        assert_eq!(report.left[0].symbol, "415");
    }

    /// A road across a picture: a strip `strip` pixels wide of paved area,
    /// down the middle, with a border band either side, or none.
    fn road(strip: usize, borders: bool) -> (Map, LabelSet, Rules, SymbolGrid, LineGrid) {
        let (map, labels, rules) = almost();
        let size = 120;
        let paved = area(&labels, "Paved area, light traffic");
        let black = line(&labels, "Black line 0.10 mm");
        let mut grid = SymbolGrid::new(size, size);
        fill(&mut grid, area(&labels, "Forest"), 0..size, 0..size);
        let top = 60 - strip / 2;
        fill(&mut grid, paved, 0..size, top..top + strip);
        let mut lines = LineGrid::new(size, size, labels.lines.len());
        if borders {
            let channel = lines.channel_mut(black);
            for column in 0..size {
                for row in [
                    top - 2,
                    top - 1,
                    top,
                    top + strip - 1,
                    top + strip,
                    top + strip + 1,
                ] {
                    channel[row * size + column] = 1;
                }
            }
        }
        (map, labels, rules, grid, lines)
    }

    #[test]
    fn a_strip_with_borders_is_a_road_of_its_width() {
        // At 12 px to the mm, 2 m of ground at 1:4000 is 0.5 mm: 6 pixels.
        for (strip, expected) in [(4, "501.6"), (6, "501.7"), (9, "501.8"), (12, "501.9")] {
            let (map, labels, rules, grid, lines) = road(strip, true);
            let done = run(&map, &labels, &rules, &grid, &lines, &[]);
            assert_eq!(done.report.strips, 1, "a {strip}-pixel strip is one road");
            assert_eq!(code(&map, &done.objects[0]), expected, "{strip} pixels");
            let forest = area(&labels, "Forest") as u16;
            assert!(
                done.grid.class.iter().all(|&c| c == forest),
                "the road's cells are the forest under it"
            );
            let black = line(&labels, "Black line 0.10 mm");
            assert!(!done.lines.expect("lines").channel(black).contains(&1));
        }
    }

    #[test]
    fn a_strip_with_no_borders_stays_ground_and_is_counted() {
        let (map, labels, rules, grid, lines) = road(6, false);
        let done = run(&map, &labels, &rules, &grid, &lines, &[]);
        assert_eq!(done.report.strips, 0);
        assert!(done.objects.is_empty());
        assert_eq!(done.grid.class, grid.class);
        assert_eq!(done.report.left.len(), 1);
        assert_eq!(done.report.left[0].kind, LeftoverKind::Strip);
        assert_eq!(done.report.left[0].symbol, "501.5");
    }

    #[test]
    fn a_road_into_a_square_leaves_the_square_and_reaches_it() {
        let (map, labels, rules, mut grid, mut lines) = road(6, true);
        let paved = area(&labels, "Paved area, light traffic");
        let black = line(&labels, "Black line 0.10 mm");
        // The road from the left edge into an outlined square, which takes
        // the right of the picture where the road was.
        fill(&mut grid, area(&labels, "Forest"), 70..120, 0..120);
        fill(&mut grid, paved, 70..110, 40..80);
        for row in 0..120 {
            for column in 70..120 {
                lines.channel_mut(black)[row * 120 + column] = 0;
            }
        }
        ring(&mut lines, black, 70..110, 40..80, 3);
        let done = run(&map, &labels, &rules, &grid, &lines, &[]);
        assert_eq!(done.report.strips, 1);
        assert_eq!(code(&map, &done.objects[0]), "501.7");
        assert_eq!(
            done.grid.class[60 * 120 + 90],
            paved as u16,
            "the square stays"
        );
        assert_eq!(
            done.areas[done.chosen[60 * 120 + 90] as usize].code.trim(),
            "501"
        );
        // Pixel x is -20 m + x / 3 of ground, a quarter of that in mm at
        // 1:4000: the road's ends are taken to the picture's edge and to
        // the square, rather than stopping half its width short of both.
        let xs: Vec<f64> = done.objects[0].coords.iter().map(|c| c.x).collect();
        let mm = |pixel: f64| (-20.0 + pixel / 3.0) / 4.0;
        let (low, high) = xs
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, h), &x| (l.min(x), h.max(x)));
        assert!(low <= mm(1.0), "reaches the left edge: {low} mm");
        assert!(high >= mm(69.0), "reaches the square: {high} mm");
    }

    #[test]
    fn a_stairway_is_told_by_how_far_apart_its_borders_are() {
        let (map, labels, rules) = almost();
        let stairway = line(&labels, "Stairway");
        let black = line(&labels, "Black line 0.10 mm");
        // At 12 px to the mm: 532.6 has its borders 3 px from the middle,
        // 532.9 6.6 px.
        for (offset, expected) in [(3usize, "532.6"), (7, "532.9")] {
            let size = 80;
            let mut grid = SymbolGrid::new(size, size);
            fill(&mut grid, area(&labels, "Open land"), 0..size, 0..size);
            let mut lines = LineGrid::new(size, size, labels.lines.len());
            for column in 10..70 {
                for row in 39..42 {
                    lines.channel_mut(stairway)[row * size + column] = 1;
                }
                for row in [40 - offset, 40 + offset] {
                    lines.channel_mut(black)[row * size + column] = 1;
                }
            }
            let done = run(&map, &labels, &rules, &grid, &lines, &[]);
            assert_eq!(done.report.bordered, 1);
            assert_eq!(code(&map, &done.objects[0]), expected, "{offset} px");
            let left = done.lines.expect("lines");
            assert!(!left.channel(stairway).contains(&1));
            assert!(!left.channel(black).contains(&1));
        }
    }

    #[test]
    fn a_slope_line_is_the_one_of_the_contour_it_sits_on() {
        let (map, labels, rules) = almost();
        let index = line(&labels, "Index contour");
        let size = 60;
        let mut grid = SymbolGrid::new(size, size);
        fill(&mut grid, area(&labels, "Open land"), 0..size, 0..size);
        let mut lines = LineGrid::new(size, size, labels.lines.len());
        for column in 0..size {
            for row in 29..32 {
                lines.channel_mut(index)[row * size + column] = 1;
            }
        }
        let slope = point(&labels, "Slope line");
        let peak = |y: f64| Peak {
            class: slope,
            x: 20.5,
            y,
            score: 1.0,
            turn: None,
        };
        let done = run(
            &map,
            &labels,
            &rules,
            &grid,
            &lines,
            &[peak(30.5), peak(5.5)],
        );
        let on = done.point_symbols[0]
            .as_ref()
            .expect("on the index contour");
        assert_eq!(on.code.trim(), "102.1");
        assert!(
            done.point_symbols[1].is_none(),
            "far from any line, its own"
        );
        assert_eq!(done.report.points, 1);
    }

    #[test]
    fn the_distance_transform_is_exact() {
        let (width, height) = (7, 5);
        let mut sources = vec![false; width * height];
        sources[2 * width + 3] = true;
        sources[4 * width] = true;
        let near = nearest(&sources, width, height);
        for y in 0..height {
            for x in 0..width {
                let a = (
                    (x as f32 - 3.0).powi(2) + (y as f32 - 2.0).powi(2),
                    2 * width + 3,
                );
                let b = ((x as f32).powi(2) + (y as f32 - 4.0).powi(2), 4 * width);
                let (d, s) = if a.0 <= b.0 { a } else { b };
                assert_eq!(near.dist2[y * width + x], d, "({x}, {y})");
                if a.0 != b.0 {
                    assert_eq!(near.source[y * width + x], s as u32, "({x}, {y})");
                }
            }
        }
        let none = nearest(&[false; 6], 3, 2);
        assert!(none.dist2.iter().all(|d| d.is_infinite()));
    }
}
