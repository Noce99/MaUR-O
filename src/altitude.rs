//! Turning a map's contours back into the ground they describe: a raster of
//! altitudes, one number of meters per pixel.
//!
//! A contour map is a height field with almost all of the height thrown away.
//! What survives is a set of lines, each of which is level, and the knowledge
//! that neighbouring lines are one contour interval apart. What is missing is
//! the two things this module has to put back: *which way* is up, and what
//! happens between the lines.
//!
//! # Which way is up
//!
//! Nothing in a contour line says whether the ground rises or falls as it is
//! crossed, and a map file records no altitude at all — only that this object
//! is drawn with the "Contour" symbol. Three things say something about the
//! direction, and they are used in this order:
//!
//! 1. **Slope lines** (symbol 104): a tick drawn on the *lower* side of a
//!    contour, put there by the mapper exactly where the shape would
//!    otherwise be read the wrong way round — around a depression, or in a
//!    re-entrant. Where there is one it settles the matter outright.
//! 2. **Enclosure**: an area wrapped all the way round by one contour, and
//!    touching no edge of the map, is a summit rather than a pit. That is the
//!    default reading of a closed contour, and the reason a depression needs
//!    a slope line to say otherwise.
//! 3. **Monotony**: ground which rises does not stop rising because a contour
//!    was crossed. Where every settled contour around an area but one says
//!    "higher this way", the last one says "lower". This is what carries a
//!    single decided contour along a whole hillside.
//!
//! On a map with no slope lines and no closed contours — an excerpt of a
//! hillside, say, where every contour runs off the edge — none of the three
//! can start. The shape of the ground is still fully determined, but its
//! *sense* is not: the same lines describe a hill and the identical hollow.
//! One contour is then picked and guessed at, everything else follows from
//! it by rule 3, and a warning says so. `--invert` turns the answer over.
//!
//! # Between the lines
//!
//! Contours cut the map into bands, each bounded below by one contour and
//! above by the next. Inside a band a pixel is placed by how far it lies from
//! each of the two — the standard proportional fill, which puts the halfway
//! point of a band halfway up the interval, and reproduces a uniform slope
//! exactly.
//!
//! A band bounded on one side only — inside the innermost contour of a hill,
//! outside the outermost contour of the map, on the floor of a hollow — has
//! no second line to run towards, and is left flat at the one contour that
//! does bound it. Nothing is invented past the last thing the map said: a
//! summit becomes a plateau at its top ring rather than a spike, and a hollow
//! a floor at its rim rather than a pit of made-up depth. The price is that a
//! feature drawn with a *single* contour — the knolls and depressions an
//! orienteering map is full of — is one interval deep or high in the band
//! structure but comes out level with its own rim in the raster, since the
//! only thing known about its middle is the height of the ring around it.
//!
//! # How it is worked out
//!
//! All of the above is decided on the raster rather than on the paths, which
//! is what makes contours running off the edge of the map behave. Contours
//! are drawn into a grid as one-pixel walls; what is left over is flood
//! filled into bands; two bands with a wall between them are neighbours one
//! interval apart, and the rules above orient those neighbourings. A contour
//! that fails to divide anything — one which stops in mid-air because the map
//! was cut there — contributes no neighbouring at all, and so cannot say
//! anything wrong.
//!
//! Loose ends are closed first, in two ways. An end near the edge of the
//! raster is carried out to it: the map was cut through the contour there.
//! An end anywhere else is joined to the end which *faces* it — near, heading
//! towards it, and heading the opposite way, so that a line between the two
//! carries on both. What is *nearest* to the end of a contour is very often
//! the neighbouring contour running alongside one interval away, and joining
//! those two would weld together lines at different heights; facing is what
//! tells a gap in one line from a line that merely passes by. What was
//! sealed, joined and left open is reported in [`Mends`], and `--walls`
//! draws it.
//!
//! # What is left out
//!
//! Form lines. A form line is not a contour: it shows what the ground does
//! between two of them where a whole interval cannot, stops as soon as it
//! has, and closes nothing. It divides no ground and has ends which face
//! nothing, so it is counted and left out rather than made into a wall.

use std::collections::HashMap;
use std::path::Path;

use crate::geometry::flatten;
use crate::map::{Map, ObjectKind, Point, PointSymbol, Symbol};
use crate::xml_reader::read_xml_map;

/// The default resolution of an altitude raster, in pixels per meter on the
/// ground. One meter of ground per pixel: the detail a contour interval can
/// actually support, and a size a whole map fits in.
pub const DEFAULT_RESOLUTION: f64 = 1.0;

/// The default frame around the contours, in meters on the ground.
///
/// Nought, unlike the frame [`crate::render`] draws a map with. The grid here
/// is a topological instrument, not a picture: a contour clipped at the edge
/// of the map ends *on* the edge of the grid, where it goes on dividing one
/// band from the next, and any margin at all would leave it dangling in open
/// space with the two bands leaking round its end.
pub const DEFAULT_FRAME: f64 = 0.0;

/// How far the end of an unclosed contour is pulled to the edge of the grid,
/// in meters on the ground. See [`Settings::seal`].
pub const DEFAULT_SEAL: f64 = 3.0;

/// How much room is left around the contours whatever else is asked for, in
/// pixels. See where it is applied in [`map_to_altitude`].
const MARGIN: f64 = 2.0;

/// How far a loose contour end reaches for a neighbour to be joined to, in
/// meters on the ground. See [`Settings::bridge`].
///
/// Thirty meters is wider than contours are normally drawn apart and narrower
/// than a map is, which is what this has to be: every end left over by a map
/// cut along something other than its own bounding rectangle has a
/// neighbouring end within a contour spacing of it.
pub const DEFAULT_BRIDGE: f64 = 30.0;

/// What to make of a map, beyond the map itself.
pub struct Settings {
    /// Pixels per meter on the ground.
    pub resolution: f64,
    /// Meters of altitude between one contour and the next. `None` reads it
    /// from the map file, and fails where the file does not say — see
    /// [`equidistance_from_notes`].
    pub equidistance: Option<f64>,
    /// Meters of ground left around the contours. See [`DEFAULT_FRAME`].
    pub frame: f64,
    /// How far an unclosed contour's end may be from the edge of the grid and
    /// still be taken for a contour the map was cut through, in meters. Never
    /// less than two pixels, which is the rounding alone.
    ///
    /// Such an end is carried the rest of the way to the edge, so that the
    /// bands it divides stay divided.
    pub seal: f64,
    /// How far a contour end which reached no edge may be from another
    /// contour and still be joined to it, in meters. Nought never joins any.
    ///
    /// This is what a map cut along anything but its own bounding rectangle
    /// needs: the ends left along that cut lie inside the raster, next to one
    /// another, and each has to be joined to its neighbour for the bands
    /// between them to stay closed. See `bridge_end`.
    pub bridge: f64,
    /// The altitude the lowest pixel is given, in meters. Everything is
    /// relative — a contour map fixes differences in height, not heights —
    /// so this is where the ground is pinned.
    pub base: f64,
    /// Turn the whole answer upside down: every hill a hollow. What to reach
    /// for when a map with nothing to orient it came out the wrong way round.
    pub invert: bool,
    /// Keep a picture of the walls the contours were drawn as, with the ends
    /// this program closed marked apart from the lines the map drew. See
    /// [`AltitudeMap::walls`].
    pub walls: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            resolution: DEFAULT_RESOLUTION,
            equidistance: None,
            frame: DEFAULT_FRAME,
            seal: DEFAULT_SEAL,
            bridge: DEFAULT_BRIDGE,
            base: 0.0,
            invert: false,
            walls: false,
        }
    }
}

/// The ground a map's contours describe.
pub struct AltitudeMap {
    /// Width of the raster, in pixels.
    pub width: u32,
    /// Its height.
    pub height: u32,
    /// Pixels per meter on the ground, as asked for.
    pub resolution: f64,
    /// Meters between contours, as read or as given.
    pub equidistance: f64,
    /// The altitude of every pixel in meters, row by row from the top.
    pub altitude: Vec<f32>,
    /// Where the top left corner of the raster sits in the map's own ground
    /// coordinates, in meters, x rightwards and y *downwards* as map
    /// coordinates run.
    pub origin: (f64, f64),
    /// The lowest and the highest altitude in the raster, in meters.
    pub range: (f32, f32),
    /// How many contours were found, and how many of them ended up dividing
    /// nothing (see the module documentation).
    pub contours: (usize, usize),
    /// What had to be done to the contours' loose ends before they would
    /// divide the ground.
    pub mends: Mends,
    /// How many form lines were found and left out. A form line is not a
    /// contour: it shows what the ground does between two of them, stops as
    /// soon as it has shown it, and closes nothing.
    pub form_lines: usize,
    /// The walls the contours were drawn as, where [`Settings::walls`] asked
    /// for them: the state of the grid once every contour is down and every
    /// loose end has been closed, which is what everything after it reasons
    /// about.
    ///
    /// Black is a contour where the map drew it. Blue is an end carried out
    /// to the edge of the raster, because the map was cut through the contour
    /// there; red is an end joined to the nearest other contour, because the
    /// map was cut along something other than the raster's own rectangle. The
    /// blue and the red are the only lines in it the map did not draw, and
    /// between them they are what [`Settings::seal`] and [`Settings::bridge`]
    /// are for: too little of either and bands leak round the end of a
    /// contour into one another, too much and a contour which really does
    /// stop where it stops gets joined to something it has nothing to do
    /// with.
    ///
    /// The lines are one pixel wide, as the grid holds them, and a mend is
    /// usually only a few pixels long; raise the resolution to see them.
    pub walls: Option<image::RgbImage>,
    /// What went on that the caller should know about, the map file's own
    /// complaints included.
    pub warnings: Vec<String>,
}

/// What a contour-like symbol is, and what crossing it costs in altitude.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A contour or an index contour: one whole interval. An index contour is
    /// every fifth contour drawn thicker, not a different height.
    Contour,
    /// A form line. Recognised so that it can be left out: a form line is not
    /// a contour and does not close. It is drawn where the ground does
    /// something between two contours that a whole interval cannot show, and
    /// it stops as soon as it has shown it — so it divides nothing, has ends
    /// which face nothing, and would only ever cut a band in half for no
    /// reason. See [`AltitudeMap::form_lines`].
    FormLine,
    /// A slope line: no height of its own, but it says which way is down.
    SlopeLine,
}

impl Kind {
    /// What crossing this costs, in contour intervals. Only a contour is ever
    /// asked: the other two are never made into walls.
    fn weight(self) -> f64 {
        match self {
            Kind::Contour => 1.0,
            Kind::FormLine | Kind::SlopeLine => 0.0,
        }
    }
}

/// The symbol number with any suffix cut off: "101.1" is a contour just as
/// "101" is, and symbol sets do subdivide the numbers that way.
fn code_base(code: &str) -> &str {
    match code.find('.') {
        Some(i) => &code[..i],
        None => code,
    }
}

/// The symbol's number and its name, whichever kind of symbol it is.
fn code_and_name(symbol: &Symbol) -> (&str, &str) {
    match symbol {
        Symbol::Point(s) => (&s.code, &s.name),
        Symbol::Line(s) => (&s.code, &s.name),
        Symbol::Area(s) => (&s.code, &s.name),
        Symbol::Text(s) => (&s.code, &s.name),
        Symbol::Combined(s) => (&s.code, &s.name),
    }
}

/// What a symbol is, as far as the ground is concerned.
///
/// By symbol number first, which every ISOM and ISSprOM set agrees on, and by
/// name where the numbers say nothing — a set drawn from scratch may number
/// its symbols however it likes but will still call a contour a contour.
///
/// The kind of symbol matters as much as the name: 105 is "Contour value",
/// the little number written into an index contour, and is a text symbol
/// rather than a line. Matching on the name alone would take it for a
/// contour, and its bounding box for a piece of one.
fn kind_of(symbol: &Symbol) -> Option<Kind> {
    let (code, name) = code_and_name(symbol);
    let by_code = match code_base(code) {
        "101" | "102" => Some(Kind::Contour),
        "103" => Some(Kind::FormLine),
        "104" => Some(Kind::SlopeLine),
        _ => None,
    };
    let kind = by_code.or_else(|| {
        let lower = name.to_ascii_lowercase();
        if lower.contains("slope line") {
            Some(Kind::SlopeLine)
        } else if lower.contains("form line") || lower.contains("formline") {
            Some(Kind::FormLine)
        } else if lower.contains("contour") && !lower.contains("value") {
            Some(Kind::Contour)
        } else {
            None
        }
    })?;

    // A contour is drawn as a line and a slope line as a point symbol.
    // Anything else wearing the name is something else.
    match (kind, symbol) {
        (Kind::SlopeLine, Symbol::Point(_)) => Some(kind),
        (Kind::Contour | Kind::FormLine, Symbol::Line(_)) => Some(kind),
        _ => None,
    }
}

/// The contour interval a map file states, in meters, if it states one.
///
/// Only the map's own notes are read — what a mapper writes about the map
/// they drew, in the free text Mapper keeps under "Map notes". A line of it
/// saying "equidistance 5 m", "contour interval 2.5m", or "equidistanza 5"
/// is taken at its word.
///
/// The contour symbol's *description* is deliberately not read, although on
/// an ISOM set it does say "the standard vertical interval between contours
/// is 5 metres". That sentence is boilerplate shipped with the symbol set: it
/// is identical in a map drawn at 2.5 m and says nothing whatever about the
/// map in hand. Reading it would put a plausible, unremarkable and wrong
/// number on every altitude in the output, which is worse than asking.
pub fn equidistance_from_notes(map_file: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(map_file).ok()?;
    let start = text.find("<notes>")? + "<notes>".len();
    let end = text[start..].find("</notes>")? + start;
    let notes = text[start..end].to_ascii_lowercase();

    for key in [
        "equidistance",
        "equidistanza",
        "contour interval",
        "interval",
    ] {
        let mut from = 0;
        while let Some(at) = notes[from..].find(key) {
            let after = from + at + key.len();
            if let Some(value) = leading_number(&notes[after..]) {
                if value > 0.0 {
                    return Some(value);
                }
            }
            from = after;
        }
    }
    None
}

/// The first number in the text, skipping whatever punctuation and spacing
/// separates it from the word before it.
fn leading_number(text: &str) -> Option<f64> {
    let rest = text.trim_start_matches(|c: char| {
        c.is_whitespace() || c == ':' || c == '=' || c == '-' || c == '"'
    });
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    // A comma is a decimal point on the maps this is likeliest to meet.
    digits
        .replace(',', ".")
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
}

/// One contour, flattened and in ground meters.
struct Contour {
    kind: Kind,
    /// The polylines it is made of — one per part of the path, since a single
    /// object may hold several disconnected strokes.
    parts: Vec<(Vec<Point>, bool)>,
}

/// A slope line: where it stands, and which way it points.
struct SlopeTick {
    /// Where it sits, in ground meters. On the contour it belongs to.
    at: Point,
    /// A unit vector towards the lower ground, in the same coordinates.
    down: Point,
    /// How long the tick is drawn, in ground meters: how far from the contour
    /// the mapper meant to reach.
    length: f64,
}

/// The direction a slope line's tick sticks out in, in the symbol's own
/// coordinates, and how long it is.
///
/// Taken from the symbol's drawing rather than assumed, so that a set which
/// draws its tick some other way still reads correctly: the tip is the point
/// of the symbol furthest from where the symbol is placed, and the symbol is
/// placed on the contour with the tick reaching down the slope.
fn tick_of(symbol: &PointSymbol) -> Option<(Point, f64)> {
    let mut tip = Point::ZERO;
    let mut far = 0.0;
    for element in &symbol.elements {
        for coord in &element.object.coords {
            let length = coord.pos().length();
            if length > far {
                far = length;
                tip = coord.pos();
            }
        }
    }
    (far > 0.0).then(|| (tip.normalized(), far))
}

/// Everything the ground can be worked out from, pulled out of a map and put
/// into ground meters.
fn harvest(map: &Map) -> (Vec<Contour>, Vec<SlopeTick>, usize) {
    // Map coordinates are mm on the paper; the scale says what that is on the
    // ground. Both axes the same way round, y downwards, as the map has them.
    let meters_per_mm = map.scale_denominator as f64 / 1000.0;

    let mut contours = Vec::new();
    let mut ticks = Vec::new();
    let mut form_lines = 0;

    for object in &map.objects {
        let Some(symbol) = object.symbol_index.and_then(|i| map.symbols.get(i)) else {
            continue;
        };
        // A contour switched off in the symbol set is not on the map, and a
        // helper symbol was never part of it.
        if !symbol.is_visible() {
            continue;
        }
        let Some(kind) = kind_of(symbol) else {
            continue;
        };

        match kind {
            // Not a contour, and not treated as one. See `Kind::FormLine`.
            Kind::FormLine => form_lines += 1,
            Kind::Contour => {
                if !matches!(object.kind, ObjectKind::Path(_)) {
                    continue;
                }
                // Curves are flattened here once: everything downstream works
                // on polylines, and `flatten` is what the renderer draws the
                // very same contour with.
                let parts: Vec<(Vec<Point>, bool)> = flatten(&object.coords)
                    .into_iter()
                    .filter(|part| part.points.len() >= 2)
                    .map(|part| {
                        let points = part
                            .points
                            .iter()
                            .map(|p| Point::new(p.x * meters_per_mm, p.y * meters_per_mm))
                            .collect();
                        (points, part.closed)
                    })
                    .collect();
                if !parts.is_empty() {
                    contours.push(Contour { kind, parts });
                }
            }
            Kind::SlopeLine => {
                let Symbol::Point(point_symbol) = symbol else {
                    continue;
                };
                let (Some(first), Some((tip, length))) =
                    (object.coords.first(), tick_of(point_symbol))
                else {
                    continue;
                };
                // The renderer turns a rotatable point symbol by the negated
                // object rotation, the paper's y axis pointing down; the tick
                // has to be turned by exactly the same amount to end up where
                // it was drawn.
                let turn = if point_symbol.is_rotatable {
                    -object.rotation
                } else {
                    0.0
                };
                let (sin, cos) = turn.sin_cos();
                let down = Point::new(tip.x * cos - tip.y * sin, tip.y * cos + tip.x * sin);
                ticks.push(SlopeTick {
                    at: Point::new(first.x * meters_per_mm, first.y * meters_per_mm),
                    down,
                    length: length * meters_per_mm,
                });
            }
        }
    }

    (contours, ticks, form_lines)
}

/// No contour runs through this pixel.
const NO_CONTOUR: u32 = u32::MAX;
/// This pixel is not in any band — a contour runs through it.
const NO_BAND: u32 = u32::MAX;

/// The grid the whole thing is worked out on: which contour runs through each
/// pixel, and which band each pixel belongs to.
struct Grid {
    width: usize,
    height: usize,
    origin: (f64, f64),
    resolution: f64,
    /// The contour drawn through each pixel, [`NO_CONTOUR`] where none is.
    contour_of: Vec<u32>,
    /// What put the wall at each pixel there. Presentation only; see [`Laid`].
    laid: Vec<Laid>,
    /// The direction the wall was drawn in at each pixel, as a whole turn cut
    /// into 256, and nothing where there is no wall. A byte rather than a
    /// vector because there is one per pixel and a sixteenth of a degree is
    /// finer than a decision about which side of a line a neighbouring pixel
    /// is on will ever need.
    along: Vec<u8>,
    /// The band each pixel belongs to, [`NO_BAND`] on a contour.
    band_of: Vec<u32>,
    /// How many bands there are.
    bands: usize,
    /// Whether each band reaches the edge of the grid.
    on_edge: Vec<bool>,
    /// How many pixels each band covers.
    area: Vec<u32>,
}

impl Grid {
    fn at(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    /// The direction the wall at a pixel runs in, as a unit vector.
    fn direction(&self, at: usize) -> Point {
        let angle = self.along[at] as f64 * std::f64::consts::TAU / 256.0;
        Point::new(angle.cos(), angle.sin())
    }

    /// The pixel a point of ground falls in, as a real number so that a line
    /// between two of them can be drawn straight.
    fn pixel_of(&self, p: Point) -> (f64, f64) {
        (
            (p.x - self.origin.0) * self.resolution,
            (p.y - self.origin.1) * self.resolution,
        )
    }
}

/// What put a wall pixel where it is: the map, or this program closing a gap
/// the map left open. Kept only so that the picture `--walls` writes can tell
/// a mapper's line from a mended one; nothing decided by it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Laid {
    /// Nothing is drawn here.
    Nothing = 0,
    /// A contour, where the map drew it.
    Drawn = 1,
    /// A contour's end carried out to the edge of the raster, because the map
    /// was cut through it there. See `seal_end`.
    Sealed = 2,
    /// A contour's end joined to the nearest wall, because the map was cut
    /// along something other than the raster's own rectangle. See
    /// `bridge_end`.
    Bridged = 3,
}

/// One line's worth of drawing settings, so that the two routines below take
/// a pen rather than a fistful of loose arguments.
#[derive(Clone, Copy)]
struct Pen {
    /// The wall being drawn.
    wall: u32,
    /// What to record about how it came to be drawn.
    laid: Laid,
    /// Leave whatever is already there alone, which is how a bridge fills the
    /// gap between two contours without eating into either.
    only_free: bool,
}

/// Draws the pixel, letting the last wall to reach it keep it. Which one that
/// is matters little: a pixel two contours both run through is a pixel where
/// they touch, and either answer is as true as the other.
fn plot(grid: &mut Grid, x: i64, y: i64, pen: Pen, along: u8) {
    if x < 0 || y < 0 || x >= grid.width as i64 || y >= grid.height as i64 {
        return;
    }
    let at = grid.at(x as usize, y as usize);
    if pen.only_free && grid.contour_of[at] != NO_CONTOUR {
        return;
    }
    grid.contour_of[at] = pen.wall;
    grid.along[at] = along;
    grid.laid[at] = pen.laid;
}

/// Draws a straight line of pixels from one end to the other, Bresenham's
/// way.
///
/// The line comes out eight-connected, which is what a wall has to be to stop
/// a four-connected flood fill: the fill cannot slip through a diagonal
/// join, so a contour one pixel wide is enough to divide the bands it runs
/// between, however steeply it happens to run.
fn draw_line(grid: &mut Grid, from: (f64, f64), to: (f64, f64), pen: Pen) {
    // The way this piece of wall runs, kept with every pixel of it so that
    // which side of the wall a neighbouring band lies on can be worked out
    // later. See `find_steps`.
    let turn = (to.1 - from.1).atan2(to.0 - from.0) / std::f64::consts::TAU;
    let along = (turn.rem_euclid(1.0) * 256.0).round() as u32 as u8;
    let (mut x, mut y) = (from.0.round() as i64, from.1.round() as i64);
    let (x1, y1) = (to.0.round() as i64, to.1.round() as i64);
    let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
    let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
    let mut error = dx + dy;
    loop {
        plot(grid, x, y, pen, along);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * error;
        if e2 >= dy {
            error += dy;
            x += sx;
        }
        if e2 <= dx {
            error += dx;
            y += sy;
        }
    }
}

/// Carries the loose end of a contour to the nearest edge of the grid, where
/// it is close enough to one to have been cut there. Says whether it did.
///
/// A map is a rectangle cut out of a landscape, and the contours which reach
/// its border are cut with it. Their ends land on the border, but rounding
/// and the width of the pen can leave them a pixel or two inside it, which is
/// all it takes for the two bands the contour divides to meet round its end
/// and become one. Straight out to the border is not the line the mapper
/// drew, but over a pixel or two it is the line they would have.
fn seal_end(grid: &mut Grid, end: (f64, f64), contour: u32, seal: f64) -> bool {
    if seal <= 0.0 {
        return false;
    }
    let (x, y) = end;
    let (w, h) = (grid.width as f64 - 1.0, grid.height as f64 - 1.0);
    // The nearest of the four edges, and how far away it is.
    let candidates = [
        (x, -1.0, y),
        (w - x, w + 1.0, y),
        (y, x, -1.0),
        (h - y, x, h + 1.0),
    ];
    let mut best: Option<(f64, (f64, f64))> = None;
    for (distance, tx, ty) in candidates {
        if distance <= seal && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, (tx, ty)));
        }
    }
    match best {
        Some((_, target)) => {
            let pen = Pen {
                wall: contour,
                laid: Laid::Sealed,
                only_free: false,
            };
            draw_line(grid, end, target, pen);
            true
        }
        None => false,
    }
}

/// A loose end of a contour: where it stopped, and the way it was heading
/// when it did.
#[derive(Clone, Copy)]
struct LooseEnd {
    /// Where it stops, in pixels.
    at: (f64, f64),
    /// The way the contour was going as it ran out — a unit vector pointing
    /// away from the line, so it is the direction the contour would carry on
    /// in if it carried on at all.
    heading: (f64, f64),
    /// The wall it is an end of.
    wall: u32,
    /// Whether it found a partner and was joined to it.
    joined: bool,
}

/// The way a contour was heading as it ran out at one of its ends.
///
/// Measured over the last `back` pixels of the line rather than off its final
/// segment alone: a path's last segment is often a stub a fraction of a pixel
/// long, and the direction of that says nothing about where the contour was
/// going. Points away from the line, which is the direction it would continue
/// in.
fn heading_at_end(pixels: &[(f64, f64)], from_start: bool, back: f64) -> (f64, f64) {
    let tip = if from_start {
        pixels[0]
    } else {
        *pixels.last().unwrap()
    };
    let mut walked = 0.0;
    let mut anchor = tip;
    let inward: Box<dyn Iterator<Item = &(f64, f64)>> = if from_start {
        Box::new(pixels.iter().skip(1))
    } else {
        Box::new(pixels.iter().rev().skip(1))
    };
    for &point in inward {
        walked += (point.0 - anchor.0).hypot(point.1 - anchor.1);
        anchor = point;
        if walked >= back {
            break;
        }
    }
    let (dx, dy) = (tip.0 - anchor.0, tip.1 - anchor.1);
    let length = dx.hypot(dy);
    if length > 0.0 {
        (dx / length, dy / length)
    } else {
        (0.0, 0.0)
    }
}

/// How much of `--bridge` an end may reach across when the two ends face one
/// another as badly as they possibly could.
///
/// Facing earns reach rather than gating it. Two ends which are squarely
/// opposed, so that a line between them carries on both, may reach the whole
/// of what `--bridge` allows; two which are merely near each other, pointing
/// any old way, may reach this share of it and no further. Everything between
/// scales between the two.
///
/// That is the shape the problem actually has. A gap in one contour is the
/// common case and the one worth reaching a long way for, but two contours
/// both cut off by the same boundary end beside one another pointing the same
/// way, not facing at all, and closing *those* two is right as well — it is
/// the boundary itself being put back. What must not happen is an end being
/// joined to the *middle* of another contour, and no amount of loosening here
/// can cause that, because only ends are ever joined.
///
/// A half, so that facing squarely is worth twice the reach of not facing at
/// all. Measured on a real map: from here up to ignoring direction
/// altogether, the contours which end up dividing something climb steadily
/// while the number of places where the levels disagree does not move at all;
/// it is only at reaches well past the default that dropping direction starts
/// to make joins that the ground then argues with. So direction is left with
/// enough weight to matter and not enough to turn away a pair which is simply
/// very close.
const HUDDLE: f64 = 0.5;

/// Joins the loose ends which are evidently the two sides of one gap.
///
/// A contour stops in the middle of a map for a reason, and the reason is
/// almost always that the same contour carries on a little further along:
/// the map was cut by a boundary which is not the raster's own rectangle, or
/// the line was broken to let something else through. The end which carries
/// on from it is the one which *faces* it — near, heading towards it, and
/// heading the opposite way, so that a line drawn between the two continues
/// both.
///
/// Joining an end instead to whatever wall happens to be nearest is wrong,
/// and was the first thing this did. What is nearest to the end of a contour
/// is very often the *neighbouring* contour, one interval above or below it,
/// running alongside; welding the two together makes a junction where three
/// bands meet that should not, and the levels either side of it then disagree
/// about how far apart they are.
///
/// Ends which face nothing are left alone. A contour which really does stop
/// where it stops divides nothing, and is reported as dividing nothing, which
/// is better than being joined to a line it has no relation to.
fn pair_ends(grid: &mut Grid, walls: &mut Walls, ends: &mut [LooseEnd], reach: f64) -> usize {
    // Every pair which faces well enough to be worth considering, best first.
    let mut offers: Vec<(f64, usize, usize)> = Vec::new();
    for a in 0..ends.len() {
        for b in (a + 1)..ends.len() {
            let (one, two) = (ends[a], ends[b]);
            let (dx, dy) = (two.at.0 - one.at.0, two.at.1 - one.at.1);
            let gap = dx.hypot(dy);
            if gap <= 0.0 || gap > reach {
                continue;
            }
            let towards = (dx / gap, dy / gap);
            // Each heading for the other, and the two of them opposed. The
            // worst of the three is how squarely the pair faces, from one for
            // a gap bitten out of a single straight line down to minus one.
            let one_faces = one.heading.0 * towards.0 + one.heading.1 * towards.1;
            let two_faces = -(two.heading.0 * towards.0 + two.heading.1 * towards.1);
            let opposed = -(one.heading.0 * two.heading.0 + one.heading.1 * two.heading.1);
            let squareness = (one_faces.min(two_faces).min(opposed) + 1.0) / 2.0;

            // How far this pair is allowed to reach for one another, which is
            // what facing buys. See `HUDDLE`.
            if gap > reach * (HUDDLE + (1.0 - HUDDLE) * squareness) {
                continue;
            }
            // Facing counts for about twice what nearness does, so that a
            // true continuation is taken before a neighbour which merely
            // happens to have stopped nearby.
            offers.push((2.0 * squareness + (1.0 - gap / reach), a, b));
        }
    }
    // Best first, and by position where two offers are worth the same, so
    // that the same map is always mended the same way.
    offers.sort_by(|x, y| {
        y.0.partial_cmp(&x.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((x.1, x.2).cmp(&(y.1, y.2)))
    });

    let mut joined = 0;
    for (_, a, b) in offers {
        if ends[a].joined || ends[b].joined {
            continue;
        }
        let bridge = walls.weight.len() as u32;
        walls.weight.push(0.0);
        walls.contour.push(None);
        let pen = Pen {
            wall: bridge,
            laid: Laid::Bridged,
            only_free: true,
        };
        draw_line(grid, ends[a].at, ends[b].at, pen);
        ends[a].joined = true;
        ends[b].joined = true;
        joined += 1;
    }
    joined
}

/// The walls of the grid: the contours, and the bridges put in to close the
/// gaps where the map was cut through.
///
/// One wall per *connected curve*, not per contour object. A contour drawn as
/// one object with several separate strokes is several curves, and the whole
/// of what [`Sense`] does with a wall — that the ground on one side of it is
/// the higher, all along it — is true of a curve and not of an object.
struct Walls {
    /// What crossing each wall costs, in contour intervals, by wall number.
    /// A bridge costs nothing.
    weight: Vec<f64>,
    /// Which contour the wall is a piece of, and nothing for a bridge.
    contour: Vec<Option<usize>>,
}

impl Walls {
    fn is_bridge(&self, wall: u32) -> bool {
        self.contour[wall as usize].is_none()
    }
}

/// What had to be done to the contours before they would divide the ground:
/// how many loose ends were closed, and how.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mends {
    /// Ends carried out to the edge of the raster, the map having been cut
    /// through the contour there.
    pub sealed: usize,
    /// Pairs of ends joined to one another across a gap. Pairs, not ends: one
    /// bridge closes two of them.
    pub bridged: usize,
    /// Ends left as they were, facing nothing near enough to be the other
    /// side of a gap. A contour with one of these may well divide nothing.
    pub unmatched: usize,
}

/// Lays every contour into a fresh grid as one-pixel walls, and closes the
/// ends the map left open. Returns the walls, what had to be mended, and
/// where every loose end was and which way it pointed — that last only so
/// that the picture [`AltitudeMap::walls`] holds can show it.
fn draw_contours(
    grid: &mut Grid,
    contours: &[Contour],
    seal: f64,
    reach: f64,
) -> (Walls, Mends, Vec<LooseEnd>) {
    let mut walls = Walls {
        weight: Vec::new(),
        contour: Vec::new(),
    };

    // Far enough back that one stub of a final segment cannot set the
    // direction, near enough that a contour curving away from its end is
    // still measured where it ends rather than halfway along itself.
    let look = (reach * 0.25).clamp(2.0, 25.0);

    let mut loose: Vec<LooseEnd> = Vec::new();
    for (index, contour) in contours.iter().enumerate() {
        for (points, closed) in &contour.parts {
            let id = walls.weight.len() as u32;
            walls.weight.push(contour.kind.weight());
            walls.contour.push(Some(index));

            let pen = Pen {
                wall: id,
                laid: Laid::Drawn,
                only_free: false,
            };
            let pixels: Vec<(f64, f64)> = points.iter().map(|&p| grid.pixel_of(p)).collect();
            for pair in pixels.windows(2) {
                draw_line(grid, pair[0], pair[1], pen);
            }
            if *closed {
                // A closed part already comes back to its own first point, so
                // there is nothing to seal and nothing to join up.
                continue;
            }
            for from_start in [true, false] {
                loose.push(LooseEnd {
                    at: if from_start {
                        pixels[0]
                    } else {
                        *pixels.last().unwrap()
                    },
                    heading: heading_at_end(&pixels, from_start, look),
                    wall: id,
                    joined: false,
                });
            }
        }
    }

    // The edge of the raster first and only then one another, because an end
    // which belongs to the edge of the map belongs there whatever else
    // happens to lie near it. Both wait until every contour is down, so that
    // every end can see all of them.
    let mut open: Vec<LooseEnd> = Vec::new();
    let mut sealed = 0;
    for end in loose {
        if seal_end(grid, end.at, end.wall, seal) {
            sealed += 1;
        } else {
            open.push(end);
        }
    }

    let bridged = pair_ends(grid, &mut walls, &mut open, reach);
    let mends = Mends {
        sealed,
        bridged,
        unmatched: open.iter().filter(|end| !end.joined).count(),
    };

    (walls, mends, open)
}

/// Floods everything the contours left over into bands, four-connectedly.
fn find_bands(grid: &mut Grid) {
    grid.band_of = vec![NO_BAND; grid.width * grid.height];
    grid.bands = 0;
    grid.on_edge.clear();
    grid.area.clear();

    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start_y in 0..grid.height {
        for start_x in 0..grid.width {
            let at = grid.at(start_x, start_y);
            if grid.contour_of[at] != NO_CONTOUR || grid.band_of[at] != NO_BAND {
                continue;
            }
            let band = grid.bands as u32;
            grid.bands += 1;
            let mut on_edge = false;
            let mut area = 0u32;

            grid.band_of[at] = band;
            stack.push((start_x, start_y));
            while let Some((x, y)) = stack.pop() {
                area += 1;
                if x == 0 || y == 0 || x + 1 == grid.width || y + 1 == grid.height {
                    on_edge = true;
                }
                let mut visit = |nx: usize, ny: usize, stack: &mut Vec<(usize, usize)>| {
                    let n = ny * grid.width + nx;
                    if grid.contour_of[n] == NO_CONTOUR && grid.band_of[n] == NO_BAND {
                        grid.band_of[n] = band;
                        stack.push((nx, ny));
                    }
                };
                if x > 0 {
                    visit(x - 1, y, &mut stack);
                }
                if x + 1 < grid.width {
                    visit(x + 1, y, &mut stack);
                }
                if y > 0 {
                    visit(x, y - 1, &mut stack);
                }
                if y + 1 < grid.height {
                    visit(x, y + 1, &mut stack);
                }
            }
            grid.on_edge.push(on_edge);
            grid.area.push(area);
        }
    }
}

/// Two bands with a wall between them: one interval of altitude apart, in a
/// direction still to be settled.
struct Step {
    /// The lower-numbered band.
    a: u32,
    /// The higher-numbered one.
    b: u32,
    /// The wall which divides them.
    wall: u32,
    /// How far apart they are, in contour intervals.
    weight: f64,
    /// Whether `a` lies to the left of that wall, along the direction the
    /// contour was drawn in. What lets one settled step settle every other
    /// step of the same contour — see [`Sense`].
    a_on_left: bool,
    /// Which of the two is the higher ground, once that is known.
    higher: Option<u32>,
}

/// Finds which bands each wall divides from which, and which side of it they
/// are on.
///
/// A pixel of wall divides two bands where the pixels on opposite sides of it
/// belong to different ones — tested across, down, and both diagonals, since
/// a wall running at any angle has its two sides opposite each other in one
/// of those four directions. Every such pixel is a vote, and the wall with
/// the most votes between a given pair of bands is the one which divides
/// them: where two contours run alongside one another with a single band
/// between, both touch it, but only one is between it and its neighbour.
///
/// Each vote also says which side of the wall it was cast from, by the sign
/// of the cross product of the wall's direction with the way to the band.
/// The two sides are what [`Sense`] is about.
fn find_steps(grid: &Grid, walls: &Walls) -> (Vec<Step>, Vec<Vec<(u32, u32)>>) {
    // Votes, and how many of them put the lower-numbered band on the left.
    let mut votes: HashMap<(u32, u32, u32), (u32, i64)> = HashMap::new();
    const AXES: [(i64, i64); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

    for y in 0..grid.height as i64 {
        for x in 0..grid.width as i64 {
            let at = grid.at(x as usize, y as usize);
            let wall = grid.contour_of[at];
            if wall == NO_CONTOUR {
                continue;
            }
            let along = grid.direction(at);
            for (dx, dy) in AXES {
                let (ax, ay) = (x - dx, y - dy);
                let (bx, by) = (x + dx, y + dy);
                if ax < 0 || ay < 0 || bx >= grid.width as i64 || by >= grid.height as i64 {
                    continue;
                }
                if ay >= grid.height as i64 || by < 0 {
                    continue;
                }
                let one = grid.band_of[grid.at(ax as usize, ay as usize)];
                let two = grid.band_of[grid.at(bx as usize, by as usize)];
                if one == NO_BAND || two == NO_BAND || one == two {
                    continue;
                }
                // The way from the wall to whichever band is numbered lower,
                // and which side of the wall that puts it.
                let to_lower = if one < two {
                    Point::new(-dx as f64, -dy as f64)
                } else {
                    Point::new(dx as f64, dy as f64)
                };
                let cross = along.x * to_lower.y - along.y * to_lower.x;
                let key = (one.min(two), one.max(two), wall);
                let entry = votes.entry(key).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += if cross > 0.0 {
                    1
                } else if cross < 0.0 {
                    -1
                } else {
                    0
                };
            }
        }
    }

    // How convincing a wall's claim to a pair of bands is (a contour beats a
    // bridge, then the count), which wall it is, and which side of it the
    // lower-numbered band came out on.
    type Claim = ((bool, u32), u32, i64);

    // One step per pair of bands, credited to whichever wall divides them
    // most convincingly -- and to a contour rather than a bridge wherever
    // both run between the same two bands, since only the contour is a
    // statement about the ground.
    //
    // Which wall is credited settles which one carries the *step*, and a step
    // is a thing about the two bands rather than about the wall. What each
    // wall separates is kept as well, all of it, because a contour broken
    // into pieces has every piece running between the same two bands and only
    // one of them can be credited -- and every piece is still a line of that
    // height, still has to stand at it in the raster, and still has to seed
    // the fill on either side of it. See `contour_heights`.
    let mut separates: Vec<Vec<(u32, u32)>> = vec![Vec::new(); walls.weight.len()];
    let mut best: HashMap<(u32, u32), Claim> = HashMap::new();
    for ((a, b, wall), (count, side)) in votes {
        separates[wall as usize].push((a, b));
        let rank = (!walls.is_bridge(wall), count);
        let entry = best.entry((a, b)).or_insert((rank, wall, side));
        if rank > entry.0 {
            *entry = (rank, wall, side);
        }
    }
    for pairs in separates.iter_mut() {
        pairs.sort_unstable();
    }

    let mut steps: Vec<Step> = best
        .into_iter()
        .map(|((a, b), (_, wall, side))| Step {
            a,
            b,
            wall,
            weight: walls.weight[wall as usize],
            a_on_left: side >= 0,
            higher: None,
        })
        .collect();
    // In the order the bands were found, not the order a hash map happens to
    // hand them back. Where a map leaves the direction of a slope open,
    // *which* step gets guessed at decides which way a whole hillside comes
    // out, so an arbitrary order would mean the same map giving a different
    // answer every run -- and the answer is a file something else is going to
    // be built on.
    steps.sort_unstable_by_key(|step| (step.a, step.b));
    (steps, separates)
}

/// Reads the slope lines onto the steps they speak for: which step, and which
/// of its two bands is the higher.
///
/// A slope line stands on its contour with its tick reaching down the slope.
/// Walking out from where it stands, both along the tick and against it, ends
/// in the two bands the contour divides — and the one the tick reached is the
/// lower. Walking rather than simply taking the tip of the tick is what makes
/// this survive a tick drawn slightly too long or too short, and a contour
/// whose pixels the tick starts inside of.
fn read_slope_lines(grid: &Grid, steps: &[Step], ticks: &[SlopeTick]) -> Vec<(usize, u32)> {
    let mut index: HashMap<(u32, u32), usize> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        index.insert((step.a, step.b), i);
    }

    let mut read = Vec::new();
    for tick in ticks {
        // Far enough to clear the contour and reach the band beyond it, with
        // the tick's own length as the measure of what the mapper meant.
        let reach = (tick.length * grid.resolution).max(2.0) * 3.0;
        let reach = reach.min(grid.width.max(grid.height) as f64) as i64;

        let band_towards = |sign: f64| -> Option<u32> {
            let start = grid.pixel_of(tick.at);
            for step in 1..=reach {
                let x = (start.0 + tick.down.x * sign * step as f64).round();
                let y = (start.1 + tick.down.y * sign * step as f64).round();
                if x < 0.0 || y < 0.0 || x >= grid.width as f64 || y >= grid.height as f64 {
                    return None;
                }
                let band = grid.band_of[grid.at(x as usize, y as usize)];
                if band != NO_BAND {
                    return Some(band);
                }
            }
            None
        };

        let (Some(low), Some(high)) = (band_towards(1.0), band_towards(-1.0)) else {
            continue;
        };
        if low == high {
            continue;
        }
        if let Some(&i) = index.get(&(low.min(high), low.max(high))) {
            read.push((i, high));
        }
    }
    read
}

/// How far apart the bands stand, built up as the steps between them are
/// settled.
///
/// A weighted union-find. Bands joined by a settled step are in one set, and
/// each carries how far above the root of that set it stands. Two bands
/// already in the same set have a height difference which is already fixed,
/// whichever way round the map was walked to fix it — which is what settles a
/// step that closes a loop, and why no loop can ever be settled twice into a
/// contradiction.
struct Levels {
    parent: Vec<usize>,
    /// How far this band stands above its parent, in contour intervals.
    above: Vec<f64>,
}

impl Levels {
    fn new(bands: usize) -> Levels {
        Levels {
            parent: (0..bands).collect(),
            above: vec![0.0; bands],
        }
    }

    /// The set a band belongs to, and how far it stands above that set's
    /// root. Flattens the chain it walked on the way back, offsets and all.
    fn root(&mut self, band: usize) -> (usize, f64) {
        let mut chain = Vec::new();
        let mut at = band;
        while self.parent[at] != at {
            chain.push(at);
            at = self.parent[at];
        }
        let root = at;
        let mut above = 0.0;
        for &node in chain.iter().rev() {
            above += self.above[node];
            self.above[node] = above;
            self.parent[node] = root;
        }
        (root, if band == root { 0.0 } else { self.above[band] })
    }

    /// How far `b` stands above `a`, where the two are already related.
    fn difference(&mut self, a: usize, b: usize) -> Option<f64> {
        let (root_a, above_a) = self.root(a);
        let (root_b, above_b) = self.root(b);
        (root_a == root_b).then_some(above_b - above_a)
    }

    /// Settles `b` as standing `rise` above `a`, where it is not already
    /// settled the other way about.
    fn join(&mut self, a: usize, b: usize, rise: f64) {
        let (root_a, above_a) = self.root(a);
        let (root_b, above_b) = self.root(b);
        if root_a == root_b {
            return;
        }
        self.parent[root_b] = root_a;
        self.above[root_b] = above_a + rise - above_b;
    }

    /// The height of every band, each set counted from its own root.
    fn heights(&mut self) -> Vec<f64> {
        (0..self.parent.len()).map(|b| self.root(b).1).collect()
    }
}

/// Which side of a wall is the higher ground, and what follows from that.
///
/// The whole of the difficulty is here. A contour says that the ground is
/// level along it and nothing about which way it falls away, and a map file
/// says less still. What settles it:
///
/// * **A contour has one uphill side.** It is a line of constant height on a
///   continuous surface, so the ground to its left is above it everywhere or
///   below it everywhere — never one and then the other. So a single settled
///   step settles every other step of that same contour, however far along it
///   they lie and whatever bands they run between. This is what does most of
///   the work, and it is why the side each step was voted from is recorded.
/// * **A loop settles itself.** Where a step's two bands are already related
///   by some other way round the map, that way round has already fixed which
///   is the higher, and there is nothing left to choose.
/// * **Slope lines**, where the mapper drew them, and **enclosure** — a band
///   wrapped in one contour and touching no edge of the map is a summit.
/// * **Monotony**, last: where every settled contour around a band but one
///   leads the same way, the last leads the other way.
///
/// Where all of that runs out the map genuinely does not say, and one step is
/// picked and guessed at. Every guess is counted and reported, because a
/// guess is not a small error but a whole hillside upside down.
struct Sense {
    /// For each wall, whether the ground to its left is the higher.
    left_high: Vec<Option<bool>>,
    levels: Levels,
    guesses: usize,
    contradictions: usize,
}

impl Sense {
    /// Settles one step, and everything that follows from it: which side of
    /// its wall is up, and how far its two bands stand apart.
    fn settle(&mut self, steps: &mut [Step], i: usize, higher: u32) {
        let step = &mut steps[i];
        step.higher = Some(higher);
        let a_high = higher == step.a;
        self.left_high[step.wall as usize] = Some(a_high == step.a_on_left);
        let rise = if a_high { -step.weight } else { step.weight };
        self.levels.join(step.a as usize, step.b as usize, rise);
    }

    /// Settles a step from what is already known, if anything is. Says
    /// whether it did.
    fn deduce(&mut self, steps: &mut [Step], i: usize) -> bool {
        if steps[i].higher.is_some() {
            return false;
        }
        // The wall has an uphill side already, so this step has one too.
        if let Some(left_high) = self.left_high[steps[i].wall as usize] {
            let higher = if left_high == steps[i].a_on_left {
                steps[i].a
            } else {
                steps[i].b
            };
            self.settle(steps, i, higher);
            return true;
        }
        // The two bands are already related the long way round.
        let (a, b) = (steps[i].a as usize, steps[i].b as usize);
        if let Some(rise) = self.levels.difference(a, b) {
            if (rise.abs() - steps[i].weight).abs() > 1e-9 {
                self.contradictions += 1;
            }
            let higher = if rise >= 0.0 { steps[i].b } else { steps[i].a };
            self.settle(steps, i, higher);
            return true;
        }
        false
    }
}

/// Settles which side of every step is the higher ground, and how high each
/// band stands. See [`Sense`] for the reasoning; this is the order it is
/// applied in.
fn orient(
    grid: &Grid,
    walls: &Walls,
    steps: &mut [Step],
    slope_lines: &[(usize, u32)],
) -> (Vec<f64>, usize, usize) {
    let mut touching: Vec<Vec<usize>> = vec![Vec::new(); grid.bands];
    for (i, step) in steps.iter().enumerate() {
        touching[step.a as usize].push(i);
        touching[step.b as usize].push(i);
    }

    let mut sense = Sense {
        left_high: vec![None; walls.weight.len()],
        levels: Levels::new(grid.bands),
        guesses: 0,
        contradictions: 0,
    };

    // A bridge divides the grid without dividing the ground: its two sides
    // are the same height, and there is nothing to settle about it.
    for i in 0..steps.len() {
        if steps[i].weight == 0.0 {
            let a = steps[i].a;
            sense.settle(steps, i, a);
        }
    }

    // The mapper's own word first, where they gave it.
    for &(i, higher) in slope_lines {
        if steps[i].higher.is_some() {
            if steps[i].higher != Some(higher) {
                sense.contradictions += 1;
            }
            continue;
        }
        sense.settle(steps, i, higher);
    }

    // A band wrapped in a single contour and touching no edge of the map is
    // a summit. This is only the *default* reading of a closed contour,
    // though -- the reading a depression is drawn with a slope line to
    // overturn -- so it waits below until everything the map actually said
    // has been followed out as far as it goes.
    let enclosed = |steps: &[Step]| -> Option<(usize, usize)> {
        (0..grid.bands).find_map(|band| {
            if grid.on_edge[band] {
                return None;
            }
            // Bridges are not counted: a summit cut in two by one is still a
            // summit.
            let mut wrapping = touching[band].iter().filter(|&&i| steps[i].weight > 0.0);
            match (wrapping.next(), wrapping.next()) {
                (Some(&only), None) if steps[only].higher.is_none() => Some((band, only)),
                _ => None,
            }
        })
    };

    loop {
        let mut moved = false;

        // Everything that follows from what is settled: a wall settled
        // anywhere is settled along its whole length, and a step which closes
        // a loop is settled by the way round the loop.
        for i in 0..steps.len() {
            moved |= sense.deduce(steps, i);
        }

        // Ground which rises goes on rising: where every settled contour
        // around a band but one leads the same way, the last leads the other.
        for (band, around) in touching.iter().enumerate() {
            let mut undecided = None;
            let mut up = 0;
            let mut down = 0;
            for &i in around {
                if steps[i].weight == 0.0 {
                    continue;
                }
                match steps[i].higher {
                    None if undecided.is_some() => {
                        undecided = None;
                        break;
                    }
                    None => undecided = Some(i),
                    Some(higher) if higher == band as u32 => down += 1,
                    Some(_) => up += 1,
                }
            }
            let Some(i) = undecided else { continue };
            // Higher one way and lower another is a saddle, and the last step
            // could go either way; nothing settled at all leaves nothing to
            // reason from.
            if (up > 0) == (down > 0) {
                continue;
            }
            let other = if steps[i].a == band as u32 {
                steps[i].b
            } else {
                steps[i].a
            };
            sense.settle(steps, i, if up > 0 { band as u32 } else { other });
            moved = true;
        }

        if moved {
            continue;
        }

        // Nothing more follows from what the map said. Fall back on the
        // closed contour reading before guessing outright, and let whatever
        // it settles be followed out in turn.
        if let Some((band, only)) = enclosed(steps) {
            sense.settle(steps, only, band as u32);
            continue;
        }

        // Still nothing: the map never said which way this part of it runs
        // -- no slope line, no contour closed inside it, nothing settled
        // nearby to follow out. What is left is the reading a contour map has
        // by default, that inner ground is higher ground, and the only choice
        // is where to apply it first.
        //
        // Where to apply it first is the whole of the question, because one
        // guess is carried by the rules above across everything it reaches.
        // The soundest place is the step with the most outward band on one
        // side of it: ground running off the edge of the map is the outside
        // of whatever the map drew, and the outside is the bottom of it. Only
        // when no step has one side out and one side in does this come down
        // to the sizes, a hilltop being smaller than the slope it stands on.
        let open: Vec<usize> = (0..steps.len())
            .filter(|&i| steps[i].higher.is_none())
            .collect();
        if open.is_empty() {
            break;
        }
        let anchored = open
            .iter()
            .copied()
            .filter(|&i| {
                let (a, b) = (steps[i].a as usize, steps[i].b as usize);
                grid.on_edge[a] != grid.on_edge[b]
            })
            .max_by_key(|&i| {
                let (a, b) = (steps[i].a as usize, steps[i].b as usize);
                grid.area[a].max(grid.area[b])
            });
        let i = anchored.unwrap_or(open[0]);
        let (a, b) = (steps[i].a as usize, steps[i].b as usize);
        let inner = match (grid.on_edge[a], grid.on_edge[b]) {
            (true, false) => steps[i].b,
            (false, true) => steps[i].a,
            _ if grid.area[a] <= grid.area[b] => steps[i].a,
            _ => steps[i].b,
        };
        sense.settle(steps, i, inner);
        sense.guesses += 1;
    }

    let heights = sense.levels.heights();
    (heights, sense.guesses, sense.contradictions)
}

/// The altitude of every wall, in contour intervals, and `None` for one that
/// divides nothing.
///
/// A contour between a band at *h* and one at *h+1* is the line where the
/// ground reaches *h+1*: the top of the lower band and the bottom of the
/// upper one. So it stands at the greater of the two heights, and a band at
/// *h* runs from its own *h* at the bottom to the next contour up.
///
/// Worked out from what each wall separates rather than from the steps,
/// because a step belongs to a pair of bands and only one wall can carry it.
/// A contour broken into several pieces, or mended with a bridge, has more
/// walls than there are steps between the bands they run between, and every
/// one of those walls is still a line at that height.
fn contour_heights(
    separates: &[Vec<(u32, u32)>],
    height: &[f64],
    walls: &Walls,
) -> Vec<Option<f64>> {
    let mut at = vec![None; walls.weight.len()];
    for (wall, pairs) in separates.iter().enumerate() {
        // A bridge is not a line of equal height and has none to give its
        // pixels; they are filled in afterwards from the ground around them.
        if walls.is_bridge(wall as u32) {
            continue;
        }
        for &(a, b) in pairs {
            let (a, b) = (height[a as usize], height[b as usize]);
            if a.is_nan() || b.is_nan() {
                continue;
            }
            let top = a.max(b);
            // A contour dividing several pairs of bands should divide them all
            // at the same height; where it does not, the highest wins, which
            // keeps the field increasing across it either way.
            if at[wall].is_none_or(|current: f64| top > current) {
                at[wall] = Some(top);
            }
        }
    }
    at
}

/// Distance, in fifths of a pixel, standing for "no contour of this sort was
/// reached".
const FAR: u32 = u32::MAX;
/// What a step straight across costs a chamfer distance. Five and seven are
/// the small integers whose ratio is nearest the 1:sqrt(2) a real distance
/// has, so a field grown with them is within a few percent of a true one.
const ORTHOGONAL: u32 = 5;
/// What a step across a corner costs.
const DIAGONAL: u32 = 7;

/// How far each pixel is from the contour bounding its band below and from
/// the one bounding it above, and what those two contours stand at.
///
/// Both fields are grown at once over the whole grid, in two-pass chamfer
/// sweeps. Growing them together is safe because a band is walled in by its
/// own contours: a pixel is only ever reached from inside its own band, so
/// what it finds is that band's floor and that band's ceiling rather than
/// some other band's. Contour pixels are never written to and never read
/// from, which is what makes the walls hold.
struct Fields {
    below: Vec<u32>,
    below_at: Vec<f32>,
    above: Vec<u32>,
    above_at: Vec<f32>,
}

fn grow_fields(grid: &Grid, height: &[f64], contour_at: &[Option<f64>]) -> Fields {
    let pixels = grid.width * grid.height;
    let mut fields = Fields {
        below: vec![FAR; pixels],
        below_at: vec![0.0; pixels],
        above: vec![FAR; pixels],
        above_at: vec![0.0; pixels],
    };

    // Seed: every pixel of band next to a pixel of contour. Whether that
    // contour is the band's floor or its ceiling is settled by comparing the
    // two heights, which is the whole of the bookkeeping.
    for y in 0..grid.height {
        for x in 0..grid.width {
            let at = grid.at(x, y);
            let contour = grid.contour_of[at];
            if contour == NO_CONTOUR {
                continue;
            }
            let Some(stands_at) = contour_at[contour as usize] else {
                continue;
            };
            let seed = |nx: usize, ny: usize, fields: &mut Fields| {
                let n = ny * grid.width + nx;
                let band = grid.band_of[n];
                if band == NO_BAND {
                    return;
                }
                let floor = height[band as usize];
                // One whole pixel away, so that a band one pixel thick still
                // has somewhere to interpolate between.
                if stands_at <= floor + 1e-9 {
                    if ORTHOGONAL < fields.below[n] {
                        fields.below[n] = ORTHOGONAL;
                        fields.below_at[n] = stands_at as f32;
                    }
                } else if ORTHOGONAL < fields.above[n] {
                    fields.above[n] = ORTHOGONAL;
                    fields.above_at[n] = stands_at as f32;
                }
            };
            if x > 0 {
                seed(x - 1, y, &mut fields);
            }
            if x + 1 < grid.width {
                seed(x + 1, y, &mut fields);
            }
            if y > 0 {
                seed(x, y - 1, &mut fields);
            }
            if y + 1 < grid.height {
                seed(x, y + 1, &mut fields);
            }
        }
    }

    // Two rounds of forward and backward sweeps. One round is the textbook
    // chamfer transform and is exact in open ground; a second catches the
    // places where a band wraps round a spur and the distance has to travel
    // back the way the first sweep came.
    for _ in 0..2 {
        sweep(grid, &mut fields, true);
        sweep(grid, &mut fields, false);
    }
    fields
}

/// One chamfer pass over the grid, forwards from the top left or backwards
/// from the bottom right.
fn sweep(grid: &Grid, fields: &mut Fields, forward: bool) {
    // The half of the neighbourhood a pass has already been through.
    let offsets: [(i64, i64, u32); 4] = if forward {
        [
            (-1, -1, DIAGONAL),
            (0, -1, ORTHOGONAL),
            (1, -1, DIAGONAL),
            (-1, 0, ORTHOGONAL),
        ]
    } else {
        [
            (1, 1, DIAGONAL),
            (0, 1, ORTHOGONAL),
            (-1, 1, DIAGONAL),
            (1, 0, ORTHOGONAL),
        ]
    };

    let rows: Vec<usize> = if forward {
        (0..grid.height).collect()
    } else {
        (0..grid.height).rev().collect()
    };
    let columns: Vec<usize> = if forward {
        (0..grid.width).collect()
    } else {
        (0..grid.width).rev().collect()
    };

    for &y in &rows {
        for &x in &columns {
            let at = grid.at(x, y);
            if grid.band_of[at] == NO_BAND {
                continue;
            }
            for (dx, dy, cost) in offsets {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= grid.width as i64 || ny >= grid.height as i64 {
                    continue;
                }
                let n = ny as usize * grid.width + nx as usize;
                if grid.band_of[n] == NO_BAND {
                    continue;
                }
                if fields.below[n] != FAR && fields.below[n] + cost < fields.below[at] {
                    fields.below[at] = fields.below[n] + cost;
                    fields.below_at[at] = fields.below_at[n];
                }
                if fields.above[n] != FAR && fields.above[n] + cost < fields.above[at] {
                    fields.above[at] = fields.above[n] + cost;
                    fields.above_at[at] = fields.above_at[n];
                }
            }
        }
    }
}

/// Reads a map's contours and works out the ground under them.
pub fn map_to_altitude(map_file: &Path, settings: &Settings) -> Result<AltitudeMap, String> {
    if !settings.resolution.is_finite() || settings.resolution <= 0.0 {
        return Err(format!("invalid resolution: {}", settings.resolution));
    }
    if !settings.frame.is_finite() || settings.frame < 0.0 {
        return Err(format!("invalid frame: {}", settings.frame));
    }
    if !settings.seal.is_finite() || settings.seal < 0.0 {
        return Err(format!("invalid seal: {}", settings.seal));
    }
    if !settings.bridge.is_finite() || settings.bridge < 0.0 {
        return Err(format!("invalid bridge: {}", settings.bridge));
    }

    let (map, mut warnings) =
        read_xml_map(map_file).map_err(|e| format!("cannot read {}: {e}", map_file.display()))?;

    let equidistance = match settings.equidistance {
        Some(given) if given.is_finite() && given > 0.0 => given,
        Some(given) => return Err(format!("invalid equidistance: {given}")),
        None => equidistance_from_notes(map_file).ok_or_else(|| {
            "the map does not say what its contour interval is -- give it with --equidistance"
                .to_string()
        })?,
    };

    let (contours, ticks, form_lines) = harvest(&map);
    if contours.is_empty() {
        return Err(format!("{} draws no contours", map_file.display()));
    }

    // The ground the contours cover, which is the ground there is anything to
    // be said about.
    let (mut left, mut top) = (f64::INFINITY, f64::INFINITY);
    let (mut right, mut bottom) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for contour in &contours {
        for (points, _) in &contour.parts {
            for p in points {
                left = left.min(p.x);
                right = right.max(p.x);
                top = top.min(p.y);
                bottom = bottom.max(p.y);
            }
        }
    }
    // A couple of pixels of margin, whatever else was asked for. A contour
    // which happens to run along the very edge of the ground the map drew
    // would otherwise have the edge of the raster for its outer side, with no
    // band beyond it and so nothing to divide -- and two pixels is close
    // enough that the ends of the open contours are still carried out to the
    // edge across it. See `seal_end`.
    let margin = MARGIN / settings.resolution;
    let (left, top) = (
        left - settings.frame - margin,
        top - settings.frame - margin,
    );
    let (right, bottom) = (
        right + settings.frame + margin,
        bottom + settings.frame + margin,
    );

    let width = ((right - left) * settings.resolution).ceil().max(1.0);
    let height = ((bottom - top) * settings.resolution).ceil().max(1.0);
    if width * height > 512.0 * 1024.0 * 1024.0 {
        return Err(format!(
            "an altitude raster of {width}x{height} pixels is too big; lower --resolution"
        ));
    }
    let (width, height) = (width as usize + 1, height as usize + 1);

    let mut grid = Grid {
        width,
        height,
        origin: (left, top),
        resolution: settings.resolution,
        contour_of: vec![NO_CONTOUR; width * height],
        laid: vec![Laid::Nothing; width * height],
        along: vec![0; width * height],
        band_of: Vec::new(),
        bands: 0,
        on_edge: Vec::new(),
        area: Vec::new(),
    };

    // Both distances are on the ground, but sealing to the edge is also
    // undoing the rounding that put the end there and crossing the margin
    // added above, so it never gets less than what those two take.
    let seal = (settings.seal * settings.resolution).max(MARGIN + 2.0);
    let reach = settings.bridge * settings.resolution;
    let (walls, mends, open_ends) = draw_contours(&mut grid, &contours, seal, reach);
    // Painted here rather than at the end: this is the state of the grid the
    // whole of the rest of the work reasons about, and the point of looking
    // at it is to see what that work was given.
    let picture = settings
        .walls
        .then(|| walls_picture(&grid, &open_ends, reach));

    find_bands(&mut grid);

    let (mut steps, separates) = find_steps(&grid, &walls);
    let slope_lines = read_slope_lines(&grid, &steps, &ticks);
    let heard = slope_lines.len();
    let (mut height_of, guesses, conflicts) = orient(&grid, &walls, &mut steps, &slope_lines);

    if settings.invert {
        for h in &mut height_of {
            *h = -*h;
        }
    }

    let contour_at = contour_heights(&separates, &height_of, &walls);
    let mut used = vec![false; contours.len()];
    for (wall, at) in contour_at.iter().enumerate() {
        if let (Some(_), Some(contour)) = (at, walls.contour[wall]) {
            used[contour] = true;
        }
    }
    let idle = used.iter().filter(|u| !**u).count();

    let fields = grow_fields(&grid, &height_of, &contour_at);

    // Every pixel, in contour intervals for now: a contour stands where it
    // stands, and a pixel of band is placed between the contour below it and
    // the one above by how far it lies from each.
    let mut altitude = vec![f32::NAN; width * height];
    for (at, height) in altitude.iter_mut().enumerate() {
        let contour = grid.contour_of[at];
        if contour != NO_CONTOUR {
            if let Some(stands_at) = contour_at[contour as usize] {
                *height = stands_at as f32;
            }
            continue;
        }
        let band = grid.band_of[at];
        if band == NO_BAND {
            continue;
        }
        *height = match (fields.below[at], fields.above[at]) {
            (FAR, FAR) => height_of[band as usize] as f32,
            // Outside the lowest contour, or inside the highest: there is no
            // second line to run towards, so the ground is left level at the
            // one contour which does bound it rather than made up.
            (FAR, _) => fields.above_at[at],
            (_, FAR) => fields.below_at[at],
            (below, above) => {
                let share = below as f32 / (below + above) as f32;
                fields.below_at[at] + (fields.above_at[at] - fields.below_at[at]) * share
            }
        };
    }

    // The pixels of a contour which divides nothing were never given a
    // height. Take the mean of whatever is around them, so that they are a
    // line drawn on the ground rather than a hole in it.
    fill_holes(&grid, &mut altitude);

    // Into meters, and pinned so that the lowest ground sits at --base.
    let mut lowest = f32::INFINITY;
    let mut highest = f32::NEG_INFINITY;
    for value in altitude.iter_mut() {
        if value.is_nan() {
            continue;
        }
        *value *= equidistance as f32;
        lowest = lowest.min(*value);
        highest = highest.max(*value);
    }
    if !lowest.is_finite() {
        return Err("no ground could be worked out from these contours".to_string());
    }
    let shift = settings.base as f32 - lowest;
    for value in altitude.iter_mut() {
        if value.is_nan() {
            *value = settings.base as f32;
        } else {
            *value += shift;
        }
    }

    if heard == 0 && !ticks.is_empty() {
        warnings.push(format!(
            "{} slope lines were found but none of them stood between two contour bands",
            ticks.len()
        ));
    }
    if guesses > 0 {
        warnings.push(format!(
            "nothing said which way the ground runs across {guesses} contour{}: \
             the shape is right but its sense was guessed at, and --invert turns it over",
            if guesses == 1 { "" } else { "s" }
        ));
    }
    if idle > 0 {
        warnings.push(format!(
            "{idle} of {} contours divide nothing and were ignored -- they stop inside the map \
             with no end facing theirs to carry them on. --walls draws every loose end and \
             the way it was heading, and --bridge says how far one may reach for its partner",
            contours.len()
        ));
    }
    if conflicts > 0 {
        warnings.push(format!(
            "{conflicts} contours disagree with the ground around them about how far apart \
             they are; the raster follows the ground, and a higher --resolution may separate \
             contours which have run together"
        ));
    }

    Ok(AltitudeMap {
        width: width as u32,
        height: height as u32,
        resolution: settings.resolution,
        equidistance,
        altitude,
        origin: (left, top),
        range: (lowest + shift, highest + shift),
        contours: (contours.len(), idle),
        mends,
        form_lines,
        walls: picture,
        warnings,
    })
}

/// Gives the pixels nothing reached the mean of their neighbours, over and
/// over until none are left.
fn fill_holes(grid: &Grid, altitude: &mut [f32]) {
    for _ in 0..8 {
        let mut holes = Vec::new();
        for (at, height) in altitude.iter().enumerate() {
            if height.is_nan() {
                holes.push(at);
            }
        }
        if holes.is_empty() {
            return;
        }
        let mut filled = 0;
        for &at in &holes {
            let (x, y) = (at % grid.width, at / grid.width);
            let mut sum = 0.0;
            let mut count = 0;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= grid.width as i64 || ny >= grid.height as i64 {
                        continue;
                    }
                    let value = altitude[ny as usize * grid.width + nx as usize];
                    if !value.is_nan() {
                        sum += value;
                        count += 1;
                    }
                }
            }
            if count > 0 {
                altitude[at] = sum / count as f32;
                filled += 1;
            }
        }
        if filled == 0 {
            return;
        }
    }
}

/// Draws a line of pixels onto a picture, Bresenham's way, clipped to it, and
/// only over pixels which are still `clear`.
///
/// The grid's own [`draw_line`] cannot be used: what is drawn here is
/// commentary, and putting any of it into the grid would make it a wall.
/// Leaving the pixels which are not `clear` alone is what keeps the
/// commentary from hiding what it is commenting on — an arrow across a gap
/// which was closed would otherwise paint over the very join it explains.
fn stroke(
    out: &mut image::RgbImage,
    from: (f64, f64),
    to: (f64, f64),
    colour: image::Rgb<u8>,
    clear: image::Rgb<u8>,
) {
    let (mut x, mut y) = (from.0.round() as i64, from.1.round() as i64);
    let (x1, y1) = (to.0.round() as i64, to.1.round() as i64);
    let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
    let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
    let mut error = dx + dy;
    loop {
        if x >= 0
            && y >= 0
            && x < out.width() as i64
            && y < out.height() as i64
            && *out.get_pixel(x as u32, y as u32) == clear
        {
            out.put_pixel(x as u32, y as u32, colour);
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * error;
        if e2 >= dy {
            error += dy;
            x += sx;
        }
        if e2 <= dx {
            error += dx;
            y += sy;
        }
    }
}

/// Paints the walls, in the colours [`AltitudeMap::walls`] describes, and
/// lays an arrow over every loose end showing the way that end was heading.
///
/// The arrows are what the pairing in [`pair_ends`] is decided on, so they
/// are what to look at when a gap was joined that should not have been, or
/// left open when it should not have been. They are laid only over blank
/// paper, never over a wall, so a gap closed tightly enough shows as the red
/// join rather than the arrows which earned it; an arrow standing on its own
/// in the white is an end which found nothing to carry it on.
fn walls_picture(grid: &Grid, ends: &[LooseEnd], reach: f64) -> image::RgbImage {
    const PAPER: image::Rgb<u8> = image::Rgb([255, 255, 255]);
    const CONTOUR: image::Rgb<u8> = image::Rgb([26, 26, 26]);
    const SEAL: image::Rgb<u8> = image::Rgb([31, 111, 208]);
    const BRIDGE: image::Rgb<u8> = image::Rgb([208, 48, 31]);
    const HEADING: image::Rgb<u8> = image::Rgb([240, 190, 0]);

    let mut out = image::RgbImage::new(grid.width as u32, grid.height as u32);
    for y in 0..grid.height {
        for x in 0..grid.width {
            let colour = match grid.laid[grid.at(x, y)] {
                Laid::Nothing => PAPER,
                Laid::Drawn => CONTOUR,
                Laid::Sealed => SEAL,
                Laid::Bridged => BRIDGE,
            };
            out.put_pixel(x as u32, y as u32, colour);
        }
    }

    // Long enough to read at a glance, short enough that an arrow cannot
    // reach further than the gap it is a candidate to close.
    let length = (reach * 0.35).clamp(6.0, 30.0);
    for end in ends {
        if end.heading == (0.0, 0.0) {
            continue;
        }
        let tip = (
            end.at.0 + end.heading.0 * length,
            end.at.1 + end.heading.1 * length,
        );
        stroke(&mut out, end.at, tip, HEADING, PAPER);
        // Two barbs, each swept back from the tip by thirty degrees.
        let barb = length * 0.4;
        for turn in [2.6f64, -2.6] {
            let (sin, cos) = turn.sin_cos();
            let away = (
                end.heading.0 * cos - end.heading.1 * sin,
                end.heading.1 * cos + end.heading.0 * sin,
            );
            stroke(
                &mut out,
                tip,
                (tip.0 + away.0 * barb, tip.1 + away.1 * barb),
                HEADING,
                PAPER,
            );
        }
    }
    out
}

/// Writes the raster as a single band 32-bit float TIFF, one altitude in
/// meters per pixel.
///
/// The cell size and the position of the raster go in as the GeoTIFF pixel
/// scale and tie point, so that GDAL and anything built on it read the ground
/// distances right without being told. There is no coordinate system: a map
/// file's own coordinates are millimetres on a sheet of paper, and what is
/// written here is those turned into meters of ground with the map scale, an
/// origin of the map's own and north up.
pub fn save_tiff(map: &AltitudeMap, path: &Path) -> Result<(), String> {
    use tiff::encoder::{colortype::Gray32Float, TiffEncoder};
    use tiff::tags::Tag;

    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut encoder = TiffEncoder::new(std::io::BufWriter::new(file))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut image = encoder
        .new_image::<Gray32Float>(map.width, map.height)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    let cell = 1.0 / map.resolution;
    // ModelPixelScale: the ground one pixel covers, x, y and z.
    let scale: [f64; 3] = [cell, cell, 0.0];
    // ModelTiepoint: raster (0,0,0) is at this point of the model. The model
    // has y going up, as GeoTIFF requires, and map coordinates have it going
    // down, so the map's y is negated on the way in.
    let tie: [f64; 6] = [0.0, 0.0, 0.0, map.origin.0, -map.origin.1, 0.0];
    image
        .encoder()
        .write_tag(Tag::Unknown(33550), &scale[..])
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    image
        .encoder()
        .write_tag(Tag::Unknown(33922), &tie[..])
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    image
        .write_data(&map.altitude)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The colour ramp the false colour picture is painted with: low ground
/// green, high ground brown and then bare, the way an atlas shades relief.
const RAMP: [(f32, [f32; 3]); 6] = [
    (0.00, [0.20, 0.42, 0.24]),
    (0.22, [0.44, 0.63, 0.29]),
    (0.45, [0.80, 0.79, 0.45]),
    (0.68, [0.72, 0.56, 0.34]),
    (0.86, [0.58, 0.44, 0.35]),
    (1.00, [0.96, 0.96, 0.95]),
];

fn ramp(t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    for pair in RAMP.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return [
                c0[0] + (c1[0] - c0[0]) * f,
                c0[1] + (c1[1] - c0[1]) * f,
                c0[2] + (c1[2] - c0[2]) * f,
            ];
        }
    }
    RAMP[RAMP.len() - 1].1
}

/// Paints the altitudes in false colour, for a person to look at.
///
/// Colour alone says which ground is high, but not whether it is a hill or
/// the hollow of the same shape — which is the one thing about this that can
/// come out backwards. So the ramp is laid under a hillshade, lit from the
/// north west as every relief map is lit, and a hill lit that way looks like
/// a hill at a glance.
pub fn false_color(map: &AltitudeMap) -> image::RgbImage {
    let (low, high) = map.range;
    let span = if high > low { high - low } else { 1.0 };
    let (width, height) = (map.width as usize, map.height as usize);
    // The ground between two pixels, for the slope the shading needs.
    let cell = (1.0 / map.resolution) as f32;

    // North west, forty five degrees up, which is where the light comes from
    // on a printed relief map.
    let azimuth = (315.0f32).to_radians();
    let zenith = (45.0f32).to_radians();

    let mut out = image::RgbImage::new(map.width, map.height);
    for y in 0..height {
        for x in 0..width {
            let here = map.altitude[y * width + x];
            let at = |dx: i64, dy: i64| -> f32 {
                let nx = (x as i64 + dx).clamp(0, width as i64 - 1) as usize;
                let ny = (y as i64 + dy).clamp(0, height as i64 - 1) as usize;
                map.altitude[ny * width + nx]
            };
            // Slope from the eight neighbours, Horn's way, which is what a
            // hillshade is normally computed from.
            let dz_dx = ((at(1, -1) + 2.0 * at(1, 0) + at(1, 1))
                - (at(-1, -1) + 2.0 * at(-1, 0) + at(-1, 1)))
                / (8.0 * cell);
            let dz_dy = ((at(-1, 1) + 2.0 * at(0, 1) + at(1, 1))
                - (at(-1, -1) + 2.0 * at(0, -1) + at(1, -1)))
                / (8.0 * cell);
            let slope = (dz_dx * dz_dx + dz_dy * dz_dy).sqrt().atan();
            // y runs downwards here, so the aspect is measured against -dz_dy.
            let aspect = (-dz_dy).atan2(dz_dx);
            let shade = zenith.cos() * slope.cos()
                + zenith.sin()
                    * slope.sin()
                    * (azimuth - std::f32::consts::FRAC_PI_2 - aspect).cos();
            let shade = shade.clamp(0.0, 1.0);

            let color = ramp((here - low) / span);
            // The shading darkens and lightens the colour rather than
            // replacing it, so that the height stays readable in the shadows.
            let light = 0.55 + 0.75 * shade;
            out.put_pixel(
                x as u32,
                y as u32,
                image::Rgb([
                    ((color[0] * light).clamp(0.0, 1.0) * 255.0) as u8,
                    ((color[1] * light).clamp(0.0, 1.0) * 255.0) as u8,
                    ((color[2] * light).clamp(0.0, 1.0) * 255.0) as u8,
                ]),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_numbers_name_the_contour_kinds() {
        let contour = Symbol::Line(crate::map::LineSymbol {
            code: "101".to_string(),
            name: "Contour".to_string(),
            ..Default::default()
        });
        assert_eq!(kind_of(&contour), Some(Kind::Contour));

        let index = Symbol::Line(crate::map::LineSymbol {
            code: "102".to_string(),
            name: "Index contour".to_string(),
            ..Default::default()
        });
        assert_eq!(kind_of(&index), Some(Kind::Contour));

        // A form line is recognised so that it can be told from a contour and
        // left out; it is never made into a wall. See `Kind::FormLine`.
        let form = Symbol::Line(crate::map::LineSymbol {
            code: "103".to_string(),
            name: "Form line".to_string(),
            ..Default::default()
        });
        assert_eq!(kind_of(&form), Some(Kind::FormLine));
        assert_eq!(Kind::Contour.weight(), 1.0);
    }

    #[test]
    fn a_contour_value_is_not_a_contour() {
        // 105 is the number written into an index contour, and its name says
        // "contour" as loudly as a contour's does.
        let value = Symbol::Text(crate::map::TextSymbol {
            code: "105".to_string(),
            name: "Contour value".to_string(),
            ..Default::default()
        });
        assert_eq!(kind_of(&value), None);
    }

    #[test]
    fn a_suffixed_number_is_still_a_contour() {
        let contour = Symbol::Line(crate::map::LineSymbol {
            code: "101.1".to_string(),
            name: "Contour, uncertain".to_string(),
            ..Default::default()
        });
        assert_eq!(kind_of(&contour), Some(Kind::Contour));
    }

    #[test]
    fn notes_are_read_for_a_contour_interval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.omap");
        std::fs::write(&path, "<map><notes>Equidistance: 2.5 m</notes></map>").unwrap();
        assert_eq!(equidistance_from_notes(&path), Some(2.5));

        std::fs::write(&path, "<map><notes>contour interval 5m</notes></map>").unwrap();
        assert_eq!(equidistance_from_notes(&path), Some(5.0));

        std::fs::write(&path, "<map><notes>a nice forest</notes></map>").unwrap();
        assert_eq!(equidistance_from_notes(&path), None);
    }

    #[test]
    fn the_tick_of_a_slope_line_points_at_its_tip() {
        let mut symbol = PointSymbol::default();
        let mut object = crate::map::Object::new(ObjectKind::Path(Default::default()));
        object.coords = vec![
            crate::map::Coord::new(0.0, 0.0, 0),
            crate::map::Coord::new(0.0, -0.75, 0),
        ];
        symbol.elements.push(crate::map::Element {
            symbol: Symbol::Line(Default::default()),
            object,
        });
        let (direction, length) = tick_of(&symbol).unwrap();
        assert!((direction.x - 0.0).abs() < 1e-9);
        assert!((direction.y + 1.0).abs() < 1e-9);
        assert!((length - 0.75).abs() < 1e-9);
    }
}
