//! The data model: coordinates, colors, the five symbol types, the three
//! object types, and [`Map`].
//!
//! Plain data: what a map file says, in the shape it says it. Behaviour lives
//! in [`crate::renderer`], which is the only thing that decides what any of
//! it looks like.
//!
//! Several fields the file format stores as a bare integer — a cap style, a
//! join style, a fill pattern type — are kept here as `i32` with named
//! constants rather than as Rust enums. Mapper reads them with an unchecked
//! cast and validates nothing, so a hand-edited or unusual file can carry a
//! value no name covers; every place that then looks at one either matches
//! the names it cares about and falls through to a default, or ignores it
//! entirely. Keeping the raw integer reproduces that for out-of-range values
//! too, and saves having to work out which unnamed value behaves like which
//! named one. An enum here would have to invent an answer where Mapper simply
//! carries the number through.

use std::collections::HashMap;

/// The conversion factor from native map units to millimeters.
///
/// Native units are 1/1000 mm *on the paper*, not on the ground. All lengths
/// in this program are stored in mm; the conversion happens once, while
/// reading.
pub const MM_PER_UNIT: f64 = 0.001;

/// A point in mm on the paper, doubling as a vector.
///
/// Double precision throughout, matching what the reference renderer computes
/// in: a map is a few hundred millimetres across and a symbol detail a few
/// hundredths of one, and single precision would start to show at that ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Rightwards, in mm.
    pub x: f64,
    /// Downwards, in mm. Paper coordinates, so y grows towards the bottom.
    pub y: f64,
}

impl Point {
    /// The origin, and the null vector.
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    /// A point at `x`, `y`.
    pub fn new(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    /// The dot product of the two vectors.
    pub fn dot(self, other: Point) -> f64 {
        self.x * other.x + self.y * other.y
    }

    /// The length of the vector, in mm.
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    /// The vector scaled to unit length, or the null vector.
    pub fn normalized(self) -> Point {
        let len = self.length();
        if len > 0.0 {
            self * (1.0 / len)
        } else {
            Point::ZERO
        }
    }

    /// The unit vector to the right of this direction (y axis pointing down).
    pub fn perp_right_unit(self) -> Point {
        self.normalized().perp_right()
    }

    /// Rotates the vector 90 degrees to the right, without normalizing.
    pub fn perp_right(self) -> Point {
        Point::new(-self.y, self.x)
    }

    /// Returns the vector with this direction and the given length.
    pub fn with_length(self, length: f64) -> Point {
        let current = self.length();
        if current > 0.0 {
            self * (length / current)
        } else {
            Point::ZERO
        }
    }
}

impl std::ops::Add for Point {
    type Output = Point;
    fn add(self, rhs: Point) -> Point {
        Point::new(self.x + rhs.x, self.y + rhs.y)
    }
}
impl std::ops::Sub for Point {
    type Output = Point;
    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
    }
}
impl std::ops::Neg for Point {
    type Output = Point;
    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}
impl std::ops::Mul<f64> for Point {
    type Output = Point;
    fn mul(self, rhs: f64) -> Point {
        Point::new(self.x * rhs, self.y * rhs)
    }
}
impl std::ops::Div<f64> for Point {
    type Output = Point;
    fn div(self, rhs: f64) -> Point {
        Point::new(self.x / rhs, self.y / rhs)
    }
}
impl std::ops::AddAssign for Point {
    fn add_assign(&mut self, rhs: Point) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

/// Flags attached to a path coordinate.
///
/// The values are part of the file format and must not be changed.
pub mod coord_flag {
    /// This coordinate and the next two are the control points of a cubic
    /// bezier ending at the one after them. This is how a curve is stored:
    /// not as a separate kind of segment, but as a flag on the vertex the
    /// curve starts at.
    pub const CURVE_START: i32 = 1 << 0;
    /// The last coordinate of a closed subpath. It repeats the position of
    /// that subpath's first coordinate.
    pub const CLOSE_POINT: i32 = 1 << 1;
    /// The line stops here and picks up again at the next coordinate, leaving
    /// a visible gap in one otherwise continuous object.
    pub const GAP_POINT: i32 = 1 << 2;
    /// The last coordinate of one subpath of an area, with another following:
    /// how an area carries a hole, or several separate rings.
    pub const HOLE_POINT: i32 = 1 << 4;
    /// A line symbol's dash symbol is drawn here.
    pub const DASH_POINT: i32 = 1 << 5;
}

/// A single path coordinate, in mm on the paper, with the flags that say what
/// the path does at it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coord {
    /// Rightwards, in mm on the paper.
    pub x: f64,
    /// Downwards, in mm on the paper.
    pub y: f64,
    /// A bit set of [`coord_flag`] values.
    pub flags: i32,
}

impl Coord {
    /// A coordinate at `x`, `y` with the given [`coord_flag`] bit set.
    pub fn new(x: f64, y: f64, flags: i32) -> Coord {
        Coord { x, y, flags }
    }

    /// The position alone, with the flags dropped.
    pub fn pos(&self) -> Point {
        Point::new(self.x, self.y)
    }

    /// See [`coord_flag::CURVE_START`].
    pub fn is_curve_start(&self) -> bool {
        self.flags & coord_flag::CURVE_START != 0
    }
    /// See [`coord_flag::CLOSE_POINT`].
    pub fn is_close_point(&self) -> bool {
        self.flags & coord_flag::CLOSE_POINT != 0
    }
    /// See [`coord_flag::HOLE_POINT`].
    pub fn is_hole_point(&self) -> bool {
        self.flags & coord_flag::HOLE_POINT != 0
    }
    /// See [`coord_flag::DASH_POINT`].
    pub fn is_dash_point(&self) -> bool {
        self.flags & coord_flag::DASH_POINT != 0
    }
    /// See [`coord_flag::GAP_POINT`].
    pub fn is_gap_point(&self) -> bool {
        self.flags & coord_flag::GAP_POINT != 0
    }
}

/// The coordinates of one object, in the order the path runs. Subpaths follow
/// one another, separated by their flags rather than by nesting.
pub type CoordList = Vec<Coord>;

/// A map color.
///
/// The index of a color in `Map::colors` is its priority: it determines the
/// drawing order of the whole map. Color 0 is drawn on top of all others.
///
/// Both representations of the color are kept. Drawing uses [`rgb`](Self::rgb);
/// [`cmyk`](Self::cmyk) is what an orienteering map is actually specified in --
/// a standard defines its colors as ink percentages, so checking a map against
/// one compares CMYK rather than the screen color derived from it. The spot
/// color definitions of the file format are still dropped: they matter to a
/// printing house, and to nothing here.
#[derive(Clone, Debug)]
pub struct Color {
    /// What the map calls this colour, e.g. "Upper purple".
    pub name: String,
    /// Red, green, blue, each in `[0, 1]`.
    pub rgb: (f32, f32, f32),
    /// Cyan, magenta, yellow and black ink, each in `[0, 1]`.
    ///
    /// The file's own values where it gives them, and derived from
    /// [`rgb`](Self::rgb) where it defines the colour by RGB alone -- so the
    /// two always describe the same colour, whichever way round the file put
    /// it.
    ///
    /// To full precision, where [`rgb`](Self::rgb) is single: this is what a
    /// map is specified in, and checking one against a standard compares
    /// these to the decimal the file was written to, where a rounding of its
    /// own would decide the answer.
    pub cmyk: (f64, f64, f64, f64),
    /// Opacity in `[0, 1]`. Below 1 the colour is blended with whatever has
    /// already been drawn under it.
    pub opacity: f64,
}

impl Default for Color {
    fn default() -> Color {
        Color {
            name: String::new(),
            rgb: (0.0, 0.0, 0.0),
            cmyk: (0.0, 0.0, 0.0, 1.0),
            opacity: 1.0,
        }
    }
}

/// The priority of registration black, which is not part of the color table.
///
/// It stands for all printed colors at once, so it is drawn on top of them.
pub const REGISTRATION_PRIORITY: i32 = -900;

/// Whether a color priority refers to a color which can be drawn.
pub fn is_color(priority: i32) -> bool {
    priority >= 0 || priority == REGISTRATION_PRIORITY
}

/// Symbol type values, as stored in the file format.
///
/// Only [`crate::xml_reader`] looks at these, to decide which variant of
/// [`Symbol`] to build; past that point the variant is the type, and nothing
/// needs the number again.
pub mod symbol_type {
    /// A [`PointSymbol`](super::PointSymbol).
    pub const POINT: i32 = 1;
    /// A [`LineSymbol`](super::LineSymbol).
    pub const LINE: i32 = 2;
    /// An [`AreaSymbol`](super::AreaSymbol).
    pub const AREA: i32 = 4;
    /// A [`TextSymbol`](super::TextSymbol).
    pub const TEXT: i32 = 8;
    /// A [`CombinedSymbol`](super::CombinedSymbol).
    pub const COMBINED: i32 = 16;
}

/// One piece of a point symbol's drawing: a shape, and the symbol it is
/// drawn with.
///
/// This is how a point symbol builds anything more complicated than a dot or
/// a circle — a hut is a small filled square, a spring is a curve and a dot —
/// and it is why a symbol can nest inside another one.
pub struct Element {
    /// The symbol this piece is drawn with. Private to the element: it is in
    /// no symbol table, and nothing else can refer to it.
    pub symbol: Symbol,
    /// The shape, in coordinates relative to where the point is placed.
    pub object: Object,
}

/// A symbol for a single point: a dot, a circle, and/or a group of elements.
///
/// Each element is a miniature object with its own symbol, given in
/// coordinates relative to the point position.
#[derive(Default)]
pub struct PointSymbol {
    /// What the symbol set calls this symbol, e.g. "Marsh".
    pub name: String,
    /// The symbol number, e.g. "308". Text rather than a number: it is
    /// hierarchical, and may carry a letter.
    pub code: String,
    /// Switched off in the symbol set: the symbol draws nothing at all.
    pub is_hidden: bool,
    /// A drawing aid rather than part of the printed map.
    pub is_helper_symbol: bool,
    /// Whether an object's own rotation turns this symbol with it.
    pub is_rotatable: bool,

    /// Radius of the filled dot, in mm.
    pub inner_radius: f64,
    /// Line width of the circle, in mm.
    pub outer_width: f64,
    /// Color index of the dot, -1 if there is none.
    pub inner_color: i32,
    /// Color index of the circle, -1 if there is none.
    pub outer_color: i32,
    /// Everything the symbol draws beyond the dot and the circle, positioned
    /// relative to the point.
    pub elements: Vec<Element>,
}

impl PointSymbol {
    /// An empty point symbol: no dot, no circle, no elements.
    pub fn new() -> PointSymbol {
        PointSymbol {
            inner_color: -1,
            outer_color: -1,
            ..Default::default()
        }
    }

    /// Returns true if this symbol draws nothing at all.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
            && !(is_color(self.inner_color) && self.inner_radius > 0.0)
            && !(is_color(self.outer_color) && self.outer_width > 0.0)
    }
}

/// A line drawn parallel to the main line of a [`LineSymbol`], to one side
/// of it.
///
/// This is how a road gets its casing: a wide fill in one colour with a thin
/// dark border shifted out to either edge of it.
#[derive(Clone)]
pub struct Border {
    /// Color index, -1 for none. A border with no colour is not drawn.
    pub color: i32,
    /// Line width, in mm on the paper.
    pub width: f64,
    /// How far out from the centre of the main line the border sits, in mm.
    pub shift: f64,
    /// Length of one dash, in mm. Only read when `dashed`.
    pub dash_length: f64,
    /// Length of the gap between dashes, in mm. Only read when `dashed`.
    pub break_length: f64,
    /// Whether the border is dashed rather than solid. The dashes are the
    /// border's own, independent of the main line's.
    pub dashed: bool,
}

impl Default for Border {
    fn default() -> Border {
        Border {
            color: -1,
            width: 0.0,
            shift: 0.0,
            dash_length: 2.0,
            break_length: 1.0,
            dashed: false,
        }
    }
}

impl Border {
    /// Whether this border draws anything: it needs both a colour and a width.
    pub fn is_visible(&self) -> bool {
        is_color(self.color) && self.width > 0.0
    }
}

/// Line cap values, as stored in the file format: how a line ends.
pub mod cap_style {
    /// Stops dead at the last coordinate.
    pub const FLAT: i32 = 0;
    /// A half disc past the last coordinate.
    pub const ROUND: i32 = 1;
    /// A half square past the last coordinate.
    pub const SQUARE: i32 = 2;
    /// Tapers to a point over the symbol's start or end offset. Unlike the
    /// others this changes the shape of the line, not just its tip.
    pub const POINTED: i32 = 3;
}

/// Line join values, as stored in the file format: how a corner is filled in.
pub mod join_style {
    /// The corner is cut off straight across.
    pub const BEVEL: i32 = 0;
    /// The two edges are extended until they meet, with the tip clipped on a
    /// sharp enough angle.
    pub const MITER: i32 = 1;
    /// The corner is rounded off.
    pub const ROUND: i32 = 2;
}

/// Mid symbol placement values, as stored in the file format.
pub mod mid_symbol_placement {
    /// One spot in the middle of every dash.
    pub const CENTER_OF_DASH: i32 = 0;
    /// One spot in the middle of every group of dashes.
    pub const CENTER_OF_DASH_GROUP: i32 = 1;
    /// One spot in the middle of every gap between dashes.
    pub const CENTER_OF_GAP: i32 = 2;
    /// No mid symbols at all.
    pub const NO_MID_SYMBOLS: i32 = 99;
}

/// A symbol for a line: a stroke, and any of borders, dashes and point
/// symbols placed along it.
///
/// By far the most involved of the symbol types, because a line in an
/// orienteering map is rarely just a line. A path is dashed; a fence is a
/// line with a symbol repeated along it; a road is a fill with a border down
/// each side. The fields below fall into groups — the stroke itself, the dash
/// pattern, the symbols placed along the line, and the two borders — and most
/// of them are only read when the group they belong to is switched on.
///
/// All lengths are in mm on the paper.
pub struct LineSymbol {
    /// What the symbol set calls this symbol, e.g. "Marsh".
    pub name: String,
    /// The symbol number, e.g. "308". Text rather than a number: it is
    /// hierarchical, and may carry a letter.
    pub code: String,
    /// Switched off in the symbol set: the symbol draws nothing at all.
    pub is_hidden: bool,
    /// A drawing aid rather than part of the printed map.
    pub is_helper_symbol: bool,
    /// Whether an object's own rotation turns this symbol with it.
    pub is_rotatable: bool,

    /// Color index of the stroke, -1 for none. A line with no colour still
    /// draws its borders and its point symbols.
    pub color: i32,
    /// Width of the stroke. Zero for a symbol which is only its borders and
    /// its point symbols.
    pub line_width: f64,
    /// The length below which an object of this symbol is undersized.
    ///
    /// Advisory, and unused by the renderer: Mapper reports an undersized
    /// line in its map issues panel and draws it regardless.
    pub minimum_length: f64,
    /// How far the line is shortened at its start, or, for a pointed cap, the
    /// length over which it tapers to a point.
    pub start_offset: f64,
    /// The same at the end of the line.
    pub end_offset: f64,

    // Layout of a solid line carrying mid symbols.
    /// Distance between mid symbols along a *solid* line.
    pub segment_length: f64,
    /// Distance from either end of a solid line to its first mid symbol.
    pub end_length: f64,

    // Layout of a dashed line.
    /// Length of one dash.
    pub dash_length: f64,
    /// Length of the gap between two dash groups.
    pub break_length: f64,
    /// How many dashes make up a group. 1 for an evenly dashed line; more
    /// gives the tight-pair-then-long-gap pattern of a footpath.
    pub dashes_in_group: i32,
    /// Length of the shorter gap between the dashes inside one group.
    pub in_group_break_length: f64,

    /// Distance between the mid symbols of a single spot, where there is more
    /// than one.
    pub mid_symbol_distance: f64,
    /// How many mid symbols are drawn at each spot, side by side along the
    /// line.
    pub mid_symbols_per_spot: i32,

    /// How the line ends, one of [`cap_style`].
    pub cap_style: i32,
    /// How the corners are filled in, one of [`join_style`].
    pub join_style: i32,
    /// Where mid symbols go on a dashed line, one of
    /// [`mid_symbol_placement`].
    pub mid_symbol_placement: i32,

    /// Whether the line is dashed. Decides which of the two layout groups
    /// above is read.
    pub dashed: bool,
    /// Whether the first and last dash group of a line are half groups. A
    /// closed line always uses them, whatever this says, so that the pattern
    /// meets itself cleanly where it comes back round.
    pub half_outer_dashes: bool,
    /// Whether a stretch too short for the dash pattern still gets one mid
    /// symbol, rather than none.
    pub show_at_least_one_symbol: bool,
    /// Whether the dash symbol is left off the first and last dash point.
    pub suppress_dash_symbol_at_ends: bool,
    /// Whether a dash symbol at a corner is scaled up to span the bend,
    /// instead of being drawn at its plain size.
    pub scale_dash_symbol: bool,

    /// Drawn once at the start of the line.
    pub start_symbol: Option<Box<PointSymbol>>,
    /// Repeated along the line, where `mid_symbol_placement` puts it.
    pub mid_symbol: Option<Box<PointSymbol>>,
    /// Drawn once at the end of the line.
    pub end_symbol: Option<Box<PointSymbol>>,
    /// Drawn at each coordinate the object flags as a dash point (see
    /// [`coord_flag::DASH_POINT`]) — the tick across a fence, the crossing bar
    /// of a footbridge.
    pub dash_symbol: Option<Box<PointSymbol>>,

    /// The border on the left of the line, in the direction it runs.
    pub border: Border,
    /// The border on the right. A symmetric symbol gives both the same
    /// values; the file format allows them to differ.
    pub right_border: Border,
}

impl Default for LineSymbol {
    fn default() -> LineSymbol {
        LineSymbol {
            name: String::new(),
            code: String::new(),
            is_hidden: false,
            is_helper_symbol: false,
            is_rotatable: false,
            color: -1,
            line_width: 0.0,
            minimum_length: 0.0,
            start_offset: 0.0,
            end_offset: 0.0,
            segment_length: 0.0,
            end_length: 0.0,
            dash_length: 0.0,
            break_length: 0.0,
            dashes_in_group: 1,
            in_group_break_length: 0.0,
            mid_symbol_distance: 0.0,
            mid_symbols_per_spot: 1,
            cap_style: cap_style::FLAT,
            join_style: join_style::MITER,
            mid_symbol_placement: mid_symbol_placement::CENTER_OF_DASH,
            dashed: false,
            half_outer_dashes: false,
            show_at_least_one_symbol: false,
            suppress_dash_symbol_at_ends: false,
            scale_dash_symbol: true,
            start_symbol: None,
            mid_symbol: None,
            end_symbol: None,
            dash_symbol: None,
            border: Border::default(),
            right_border: Border::default(),
        }
    }
}

/// Fill pattern type values, as stored in the file format.
pub mod fill_pattern_type {
    /// Parallel lines, drawn with the pattern's own colour and width.
    pub const LINE: i32 = 1;
    /// A grid of copies of a point symbol.
    pub const POINT: i32 = 2;
}

/// A pattern filling an area symbol: parallel lines or a grid of point
/// symbols.
///
/// This is what makes a marsh a marsh and an orchard an orchard — the
/// horizontal blue dashes and the regular green dots are patterns laid over
/// the area's plain fill and clipped to its outline. An area symbol may carry
/// several, drawn one over the other.
pub struct FillPattern {
    /// Which kind of pattern this is, one of [`fill_pattern_type`]. Decides
    /// which group of fields below is read.
    pub pattern_type: i32,
    /// 0 clipped, 1/2/3: unclipped if completely/center/partially inside.
    pub no_clipping: i32,
    /// Direction of the pattern lines, in radians.
    pub angle: f64,
    /// Distance between the pattern lines, in mm.
    pub line_spacing: f64,
    /// Offset perpendicular to the lines, in mm.
    pub line_offset: f64,
    /// Offset along the lines, in mm.
    pub offset_along_line: f64,
    /// Whether an object's own rotation turns the pattern with it.
    pub rotatable: bool,

    // A line pattern.
    /// Color index of the pattern lines, -1 for none.
    pub line_color: i32,
    /// Width of the pattern lines, in mm.
    pub line_width: f64,

    // A point pattern.
    /// Distance between the points along each line of the grid, in mm. The
    /// distance between the lines themselves is `line_spacing`.
    pub point_distance: f64,
    /// The symbol repeated across the grid.
    pub point: Option<Box<PointSymbol>>,
}

impl Default for FillPattern {
    fn default() -> FillPattern {
        FillPattern {
            pattern_type: fill_pattern_type::LINE,
            no_clipping: 0,
            angle: 0.0,
            line_spacing: 0.0,
            line_offset: 0.0,
            offset_along_line: 0.0,
            rotatable: false,
            line_color: -1,
            line_width: 0.0,
            point_distance: 0.0,
            point: None,
        }
    }
}

/// A symbol for an area: a plain fill, plus any number of patterns over it.
#[derive(Default)]
pub struct AreaSymbol {
    /// What the symbol set calls this symbol, e.g. "Marsh".
    pub name: String,
    /// The symbol number, e.g. "308". Text rather than a number: it is
    /// hierarchical, and may carry a letter.
    pub code: String,
    /// Switched off in the symbol set: the symbol draws nothing at all.
    pub is_hidden: bool,
    /// A drawing aid rather than part of the printed map.
    pub is_helper_symbol: bool,
    /// Whether an object's own rotation turns this symbol with it.
    pub is_rotatable: bool,

    /// Color index of the plain fill, -1 for none. An area with no fill
    /// colour still draws its patterns.
    pub color: i32,
    /// The area below which an object of this symbol is undersized, in the
    /// file's own unit: `0.001` of it is a square millimeter on the paper.
    ///
    /// Advisory, and so unused by the renderer: Mapper reports an undersized
    /// area and draws it anyway. Kept in the file's unit, and as the integer
    /// the file holds, so that the tools which do read it (`all_symbols`)
    /// compute with it exactly as `AreaSymbol::getMinimumArea()` does.
    pub minimum_area: i32,
    /// The patterns laid over the fill, in the order they are drawn.
    pub patterns: Vec<FillPattern>,
}

impl AreaSymbol {
    /// An area symbol with no fill colour and no patterns.
    pub fn new() -> AreaSymbol {
        AreaSymbol {
            color: -1,
            ..Default::default()
        }
    }
}

/// A symbol for a text object: which font it is set in, and what is drawn
/// around it to keep it readable over a busy map.
pub struct TextSymbol {
    /// What the symbol set calls this symbol, e.g. "Marsh".
    pub name: String,
    /// The symbol number, e.g. "308". Text rather than a number: it is
    /// hierarchical, and may carry a letter.
    pub code: String,
    /// Switched off in the symbol set: the symbol draws nothing at all.
    pub is_hidden: bool,
    /// A drawing aid rather than part of the printed map.
    pub is_helper_symbol: bool,
    /// Whether an object's own rotation turns this symbol with it.
    pub is_rotatable: bool,

    /// The font family asked for, which is resolved against the fonts the
    /// machine actually has when the text is drawn.
    pub font_family: String,
    /// Font size in mm.
    pub font_size: f64,
    /// Whether the bold weight of the family is asked for.
    pub bold: bool,
    /// Whether the italic style of the family is asked for.
    pub italic: bool,
    /// Whether the text is underlined.
    pub underline: bool,
    /// Whether the font's kerning pairs are applied.
    pub kerning: bool,

    /// Color index of the glyphs themselves, -1 for none.
    pub color: i32,
    /// Factor applied to the natural line spacing.
    pub line_spacing: f64,
    /// Extra spacing between paragraphs, in mm.
    pub paragraph_spacing: f64,
    /// Factor applied to the width of a space.
    pub character_spacing: f64,

    /// Whether anything is drawn behind the text to lift it off the map.
    pub framing: bool,
    /// Color index of that framing, -1 for none.
    pub framing_color: i32,
    /// Which kind of framing: 1 outlines the glyphs, 2 drops a shadow behind
    /// them. Anything else draws none.
    pub framing_mode: i32,
    /// Half the width of the outline, in mm. Only read in framing mode 1.
    pub framing_line_half_width: f64,
    /// How far the shadow is offset, in mm. Only read in framing mode 2.
    pub framing_shadow_x_offset: f64,
    /// The same, vertically.
    pub framing_shadow_y_offset: f64,

    /// Whether a rule is drawn under the text — how a form line or a boundary
    /// label is set off from what it labels.
    pub line_below: bool,
    /// Color index of that rule, -1 for none.
    pub line_below_color: i32,
    /// Width of the rule, in mm.
    pub line_below_width: f64,
    /// The gap between the text's baseline and the rule, in mm.
    pub line_below_distance: f64,
}

impl Default for TextSymbol {
    fn default() -> TextSymbol {
        TextSymbol {
            name: String::new(),
            code: String::new(),
            is_hidden: false,
            is_helper_symbol: false,
            is_rotatable: false,
            font_family: String::new(),
            font_size: 4.0,
            bold: false,
            italic: false,
            underline: false,
            kerning: true,
            color: -1,
            line_spacing: 1.0,
            paragraph_spacing: 0.0,
            character_spacing: 0.0,
            framing: false,
            framing_color: -1,
            framing_mode: 0,
            framing_line_half_width: 0.0,
            framing_shadow_x_offset: 0.0,
            framing_shadow_y_offset: 0.0,
            line_below: false,
            line_below_color: -1,
            line_below_width: 0.0,
            line_below_distance: 0.0,
        }
    }
}

/// A resolved part of a [`CombinedSymbol`], filled in by
/// [`Map::resolve_references`].
pub enum PartRef {
    /// The part names a symbol the file does not contain, and draws nothing.
    None,
    /// Index into [`Map::symbols`]: a symbol of the map, which other symbols
    /// and objects may use too.
    Shared(usize),
    /// Index into [`CombinedSymbol::owned_parts`]: a symbol belonging to this
    /// combined symbol alone.
    Private(usize),
}

/// A symbol which draws several other symbols over the same object.
///
/// How a symbol gets a look no single type can give it: a road is an area
/// fill combined with a line, a power line is a line combined with another
/// carrying the pylons. Each part is drawn over the object's own coordinates
/// as though it were the object's symbol.
#[derive(Default)]
pub struct CombinedSymbol {
    /// What the symbol set calls this symbol, e.g. "Marsh".
    pub name: String,
    /// The symbol number, e.g. "308". Text rather than a number: it is
    /// hierarchical, and may carry a letter.
    pub code: String,
    /// Switched off in the symbol set: the symbol draws nothing at all.
    pub is_hidden: bool,
    /// A drawing aid rather than part of the printed map.
    pub is_helper_symbol: bool,
    /// Whether an object's own rotation turns this symbol with it.
    pub is_rotatable: bool,

    /// The private parts of this symbol.
    pub owned_parts: Vec<Symbol>,
    /// Symbol id per part, -1 if private.
    pub part_ids: Vec<i32>,
    /// Resolved parts, may contain `PartRef::None`.
    pub parts: Vec<PartRef>,
}

/// A symbol describes how the objects drawn with it look. One variant per
/// symbol type the format has.
///
/// The properties every symbol shares — `name`, `code`, `is_hidden`,
/// `is_helper_symbol`, `is_rotatable` — sit on each variant's own struct
/// rather than on a wrapper around this enum. That is deliberate: symbols
/// nest, and a nested one carries its own. A [`LineSymbol`]'s start, mid, end
/// and dash symbols are each a whole [`PointSymbol`], with its own rotatability
/// independent of the line's, and a wrapper holding the shared fields outside
/// the enum would have nowhere to put them.
pub enum Symbol {
    /// A dot, a circle, or a small drawing placed at a single position.
    Point(PointSymbol),
    /// A stroke along a path, with its dashes, borders and point symbols.
    Line(LineSymbol),
    /// A fill inside a closed path, with its patterns.
    Area(AreaSymbol),
    /// Text set in a font.
    Text(TextSymbol),
    /// Several other symbols drawn over the same object.
    Combined(CombinedSymbol),
}

impl Symbol {
    /// Whether the symbol is switched off and draws nothing.
    pub fn is_hidden(&self) -> bool {
        match self {
            Symbol::Point(s) => s.is_hidden,
            Symbol::Line(s) => s.is_hidden,
            Symbol::Area(s) => s.is_hidden,
            Symbol::Text(s) => s.is_hidden,
            Symbol::Combined(s) => s.is_hidden,
        }
    }

    /// What the symbol set numbers this symbol, e.g. "308".
    ///
    /// Text rather than a number: a symbol code is hierarchical, and may
    /// carry a letter.
    pub fn code(&self) -> &str {
        match self {
            Symbol::Point(s) => &s.code,
            Symbol::Line(s) => &s.code,
            Symbol::Area(s) => &s.code,
            Symbol::Text(s) => &s.code,
            Symbol::Combined(s) => &s.code,
        }
    }

    /// What the symbol set calls this symbol, e.g. "Marsh".
    pub fn name(&self) -> &str {
        match self {
            Symbol::Point(s) => &s.name,
            Symbol::Line(s) => &s.name,
            Symbol::Area(s) => &s.name,
            Symbol::Text(s) => &s.name,
            Symbol::Combined(s) => &s.name,
        }
    }

    /// Whether the symbol is a drawing aid rather than part of the map.
    pub fn is_helper_symbol(&self) -> bool {
        match self {
            Symbol::Point(s) => s.is_helper_symbol,
            Symbol::Line(s) => s.is_helper_symbol,
            Symbol::Area(s) => s.is_helper_symbol,
            Symbol::Text(s) => s.is_helper_symbol,
            Symbol::Combined(s) => s.is_helper_symbol,
        }
    }

    /// Returns true if objects using this symbol must not be drawn.
    pub fn is_visible(&self) -> bool {
        !self.is_hidden() && !self.is_helper_symbol()
    }
}

/// Horizontal alignment values, as stored in the file format.
pub mod h_align {
    /// The anchor is at the left edge of the text.
    pub const LEFT: i32 = 0;
    /// The anchor is at the horizontal middle of the text.
    pub const HCENTER: i32 = 1;
    /// The anchor is at the right edge of the text.
    pub const RIGHT: i32 = 2;
}

/// Vertical alignment values, as stored in the file format: where the
/// object's anchor sits relative to the text.
pub mod v_align {
    /// The anchor is on the baseline of the first line.
    pub const BASELINE: i32 = 0;
    /// The anchor is at the top of the text.
    pub const TOP: i32 = 1;
    /// The anchor is at the vertical middle of the text.
    pub const VCENTER: i32 = 2;
    /// The anchor is at the bottom of the text.
    pub const BOTTOM: i32 = 3;
}

/// What a path or area object carries beyond its coordinates.
#[derive(Default)]
pub struct PathObject {
    /// Rotation of the area symbol's fill patterns on this object, in
    /// radians. Only read for a pattern which is `rotatable`.
    pub pattern_rotation: f64,
    /// The point the fill patterns are laid out from, in mm on the paper.
    pub pattern_origin: Point,
}

/// What a text object carries beyond its coordinates.
pub struct TextObject {
    /// The text itself. May hold several lines.
    pub text: String,
    /// Where the anchor sits horizontally, one of [`h_align`].
    pub h_align: i32,
    /// Where the anchor sits vertically, one of [`v_align`].
    pub v_align: i32,
    /// The size of the object's box, in mm, for one carrying a second
    /// coordinate: `None` for one with a single anchor point instead.
    /// `TextObject::hasSingleAnchor()`'s absence shifts a `Top` or
    /// `Baseline`-anchored line up, and a `Bottom`-anchored one down, by
    /// half the box height, so the box does not just widen or narrow the
    /// text -- it moves it, even where nothing wraps.
    pub box_size: Option<(f64, f64)>,
}

impl Default for TextObject {
    fn default() -> TextObject {
        TextObject {
            text: String::new(),
            h_align: h_align::LEFT,
            v_align: v_align::BASELINE,
            box_size: None,
        }
    }
}

/// What kind of object this is, and whatever that kind carries beyond its
/// coordinates.
pub enum ObjectKind {
    /// A single position, drawn with a point or text symbol. Carries nothing
    /// extra.
    Point,
    /// A path or an area. See [`PathObject`].
    Path(PathObject),
    /// A piece of text. See [`TextObject`].
    Text(TextObject),
}

/// Anything the map draws: coordinates, plus the symbol they are drawn with.
pub struct Object {
    /// Which kind of object, and its kind-specific data.
    pub kind: ObjectKind,
    /// Symbol id from the file, -1 if embedded.
    pub symbol_id: i32,
    /// Index into [`Map::symbols`], filled in by [`Map::resolve_references`].
    /// `None` where the id names no symbol in the file.
    ///
    /// Only objects held directly by [`Map::objects`] have one. An [`Element`]
    /// of a point symbol carries its symbol in [`Element::symbol`] instead,
    /// since that symbol is private to the element and is in no symbol table
    /// to be indexed into.
    pub symbol_index: Option<usize>,
    /// The object's own coordinates, in mm on the paper.
    pub coords: CoordList,
    /// Rotation in radians, for rotatable symbols.
    pub rotation: f64,
}

impl Object {
    /// An object of the given kind: no symbol, no coordinates, no rotation.
    pub fn new(kind: ObjectKind) -> Object {
        Object {
            kind,
            symbol_id: -1,
            symbol_index: None,
            coords: Vec::new(),
            rotation: 0.0,
        }
    }
}

/// A map: colors, symbols, and the objects of all map parts.
///
/// Map parts are merged: they are a course setting feature which does not
/// affect the rendered result.
#[derive(Default)]
pub struct Map {
    /// The map scale: 15000 for a 1:15000 map. What relates the paper
    /// millimetres everything here is measured in to meters on the ground.
    pub scale_denominator: i32,
    /// The colour table, in drawing order. A colour's index in it is its
    /// priority, and index 0 is drawn on top of everything else.
    pub colors: Vec<Color>,
    /// The symbol set, in the order the file gives it.
    pub symbols: Vec<Symbol>,
    /// The file id of each symbol, parallel to `symbols`. What an object's
    /// `symbol_id` refers to, and what [`Map::resolve_references`] resolves
    /// against.
    pub symbol_ids: Vec<i32>,
    /// Everything drawn on the map, in the order the file gives it — which is
    /// not the order it is drawn in; see [`colors`](Self::colors).
    pub objects: Vec<Object>,
}

impl Map {
    /// An empty map at the 1:15000 default scale.
    pub fn new() -> Map {
        Map {
            scale_denominator: 15000,
            ..Default::default()
        }
    }

    /// Returns the color for the given priority, or `None`.
    pub fn color(&self, priority: i32) -> Option<&Color> {
        if priority == REGISTRATION_PRIORITY {
            // Registration black is not stored in the file; it is opaque
            // black, drawn on top of everything else.
            return Some(&REGISTRATION_BLACK);
        }
        if priority < 0 {
            return None;
        }
        self.colors.get(priority as usize)
    }

    /// Resolves the symbol references of objects and combined symbols.
    pub fn resolve_references(&mut self) {
        let mut by_id: HashMap<i32, usize> = HashMap::with_capacity(self.symbols.len());
        for (i, &id) in self.symbol_ids.iter().enumerate() {
            if id >= 0 {
                by_id.insert(id, i);
            }
        }

        for symbol in &mut self.symbols {
            if let Symbol::Combined(combined) = symbol {
                let mut owned_index = 0usize;
                let mut parts = Vec::with_capacity(combined.part_ids.len());
                for &id in &combined.part_ids {
                    if id >= 0 {
                        parts.push(
                            by_id
                                .get(&id)
                                .map(|&i| PartRef::Shared(i))
                                .unwrap_or(PartRef::None),
                        );
                    } else if owned_index < combined.owned_parts.len() {
                        parts.push(PartRef::Private(owned_index));
                        owned_index += 1;
                    } else {
                        parts.push(PartRef::None);
                    }
                }
                combined.parts = parts;
            }
        }

        for object in &mut self.objects {
            object.symbol_index = by_id.get(&object.symbol_id).copied();
        }
    }
}

/// Registration black: opaque black, not part of the color table.
static REGISTRATION_BLACK: std::sync::LazyLock<Color> = std::sync::LazyLock::new(|| Color {
    name: "Registration black".to_string(),
    rgb: (0.0, 0.0, 0.0),
    // Every plate at full ink: that is what makes it register.
    cmyk: (1.0, 1.0, 1.0, 1.0),
    opacity: 1.0,
});
