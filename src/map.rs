//! The data model: coordinates, colors, the five symbol types, the three
//! object types, and [`Map`].
//!
//! Plain data; behaviour lives in the renderer. Ported from `map.h`/`map.cpp`.
//!
//! Several fields which are C++ enums stored via an unchecked cast from a
//! file integer (`LineSymbol::CapStyle(attrInt(...))` and friends) are kept
//! here as plain `i32` with named constants rather than Rust enums: the
//! original performs no validation on these casts, and later code either
//! matches specific named values (falling through to a default for anything
//! else) or switches with a fallthrough default. Preserving the raw integer
//! reproduces that exactly, including for out-of-range values from odd
//! files, without having to re-derive which unnamed values are
//! behaviourally equivalent to which named one.

use std::collections::HashMap;

/// The conversion factor from native map units to millimeters.
///
/// Native units are 1/1000 mm *on the paper*, not on the ground. All lengths
/// in this program are stored in mm; the conversion happens once, while
/// reading.
pub const MM_PER_UNIT: f64 = 0.001;

/// A point in mm on the paper. Mirrors `QPointF` (double precision).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    pub fn dot(self, other: Point) -> f64 {
        self.x * other.x + self.y * other.y
    }

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
    pub const CURVE_START: i32 = 1 << 0;
    pub const CLOSE_POINT: i32 = 1 << 1;
    pub const GAP_POINT: i32 = 1 << 2;
    pub const HOLE_POINT: i32 = 1 << 4;
    pub const DASH_POINT: i32 = 1 << 5;
}

/// A single path coordinate, in mm on the paper.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coord {
    pub x: f64,
    pub y: f64,
    pub flags: i32,
}

impl Coord {
    pub fn new(x: f64, y: f64, flags: i32) -> Coord {
        Coord { x, y, flags }
    }

    pub fn pos(&self) -> Point {
        Point::new(self.x, self.y)
    }

    pub fn is_curve_start(&self) -> bool {
        self.flags & coord_flag::CURVE_START != 0
    }
    pub fn is_close_point(&self) -> bool {
        self.flags & coord_flag::CLOSE_POINT != 0
    }
    pub fn is_hole_point(&self) -> bool {
        self.flags & coord_flag::HOLE_POINT != 0
    }
    pub fn is_dash_point(&self) -> bool {
        self.flags & coord_flag::DASH_POINT != 0
    }
    pub fn is_gap_point(&self) -> bool {
        self.flags & coord_flag::GAP_POINT != 0
    }
}

pub type CoordList = Vec<Coord>;

/// A map color.
///
/// The index of a color in `Map::colors` is its priority: it determines the
/// drawing order of the whole map. Color 0 is drawn on top of all others.
/// Only the RGB representation is kept; the CMYK and spot color definitions
/// of the file format matter for printing, not for a raster image.
#[derive(Clone, Debug)]
pub struct Color {
    pub name: String,
    /// Red, green, blue, each in `[0, 1]`.
    pub rgb: (f32, f32, f32),
    pub opacity: f64,
}

impl Default for Color {
    fn default() -> Color {
        Color {
            name: String::new(),
            rgb: (0.0, 0.0, 0.0),
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

/// Symbol type values, as stored in the file format (used only while
/// dispatching during XML parsing; each variant of [`Symbol`] already
/// carries its own data, unlike the C++ class hierarchy).
pub mod symbol_type {
    pub const POINT: i32 = 1;
    pub const LINE: i32 = 2;
    pub const AREA: i32 = 4;
    pub const TEXT: i32 = 8;
    pub const COMBINED: i32 = 16;
}

/// An object drawn relative to the position of a point symbol.
pub struct Element {
    pub symbol: Symbol,
    pub object: Object,
}

/// A symbol for a single point: a dot, a circle, and/or a group of elements.
///
/// Each element is a miniature object with its own symbol, given in
/// coordinates relative to the point position.
#[derive(Default)]
pub struct PointSymbol {
    pub name: String,
    pub code: String,
    pub is_hidden: bool,
    pub is_helper_symbol: bool,
    pub is_rotatable: bool,

    /// Radius of the filled dot, in mm.
    pub inner_radius: f64,
    /// Line width of the circle, in mm.
    pub outer_width: f64,
    /// Color index of the dot, -1 if there is none.
    pub inner_color: i32,
    /// Color index of the circle, -1 if there is none.
    pub outer_color: i32,
    pub elements: Vec<Element>,
}

impl PointSymbol {
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

/// A line drawn parallel to the main line of a `LineSymbol`.
#[derive(Clone)]
pub struct Border {
    pub color: i32,
    pub width: f64,
    pub shift: f64,
    pub dash_length: f64,
    pub break_length: f64,
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
    pub fn is_visible(&self) -> bool {
        is_color(self.color) && self.width > 0.0
    }
}

/// Cap style values, as stored in the file format.
pub mod cap_style {
    pub const FLAT: i32 = 0;
    pub const ROUND: i32 = 1;
    pub const SQUARE: i32 = 2;
    pub const POINTED: i32 = 3;
}

/// Join style values, as stored in the file format.
pub mod join_style {
    pub const BEVEL: i32 = 0;
    pub const MITER: i32 = 1;
    pub const ROUND: i32 = 2;
}

/// Mid symbol placement values, as stored in the file format.
pub mod mid_symbol_placement {
    pub const CENTER_OF_DASH: i32 = 0;
    pub const CENTER_OF_DASH_GROUP: i32 = 1;
    pub const CENTER_OF_GAP: i32 = 2;
    pub const NO_MID_SYMBOLS: i32 = 99;
}

/// A symbol for a line: a stroke, optional borders, dashes and point symbols.
pub struct LineSymbol {
    pub name: String,
    pub code: String,
    pub is_hidden: bool,
    pub is_helper_symbol: bool,
    pub is_rotatable: bool,

    pub color: i32,
    pub line_width: f64,
    pub minimum_length: f64,
    pub start_offset: f64,
    pub end_offset: f64,

    // Layout of a solid line carrying mid symbols
    pub segment_length: f64,
    pub end_length: f64,

    // Layout of a dashed line
    pub dash_length: f64,
    pub break_length: f64,
    pub dashes_in_group: i32,
    pub in_group_break_length: f64,

    pub mid_symbol_distance: f64,
    pub mid_symbols_per_spot: i32,

    pub cap_style: i32,
    pub join_style: i32,
    pub mid_symbol_placement: i32,

    pub dashed: bool,
    pub half_outer_dashes: bool,
    pub show_at_least_one_symbol: bool,
    pub suppress_dash_symbol_at_ends: bool,
    pub scale_dash_symbol: bool,

    pub start_symbol: Option<Box<PointSymbol>>,
    pub mid_symbol: Option<Box<PointSymbol>>,
    pub end_symbol: Option<Box<PointSymbol>>,
    pub dash_symbol: Option<Box<PointSymbol>>,

    pub border: Border,
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

/// Pattern type values, as stored in the file format.
pub mod fill_pattern_type {
    pub const LINE: i32 = 1;
    pub const POINT: i32 = 2;
}

/// A pattern filling an area symbol: parallel lines or a grid of point
/// symbols.
pub struct FillPattern {
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
    pub rotatable: bool,

    // LinePattern
    pub line_color: i32,
    pub line_width: f64,

    // PointPattern
    pub point_distance: f64,
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

/// A symbol for an area: a plain fill plus any number of patterns.
#[derive(Default)]
pub struct AreaSymbol {
    pub name: String,
    pub code: String,
    pub is_hidden: bool,
    pub is_helper_symbol: bool,
    pub is_rotatable: bool,

    pub color: i32,
    /// The area below which an object of this symbol is undersized, in the
    /// file's own unit: `0.001` of it is a square millimeter on the paper.
    ///
    /// Advisory, and so unused by the renderer: Mapper reports an undersized
    /// area and draws it anyway. Kept in the file's unit, and as the integer
    /// the file holds, so that the tools which do read it (`all_symbols`)
    /// compute with it exactly as `AreaSymbol::getMinimumArea()` does.
    pub minimum_area: i32,
    pub patterns: Vec<FillPattern>,
}

impl AreaSymbol {
    pub fn new() -> AreaSymbol {
        AreaSymbol {
            color: -1,
            ..Default::default()
        }
    }
}

/// A symbol for a text object.
pub struct TextSymbol {
    pub name: String,
    pub code: String,
    pub is_hidden: bool,
    pub is_helper_symbol: bool,
    pub is_rotatable: bool,

    pub font_family: String,
    /// Font size in mm.
    pub font_size: f64,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub kerning: bool,

    pub color: i32,
    /// Factor applied to the natural line spacing.
    pub line_spacing: f64,
    /// Extra spacing between paragraphs, in mm.
    pub paragraph_spacing: f64,
    /// Factor applied to the width of a space.
    pub character_spacing: f64,

    pub framing: bool,
    pub framing_color: i32,
    /// 1: line, 2: shadow.
    pub framing_mode: i32,
    pub framing_line_half_width: f64,
    pub framing_shadow_x_offset: f64,
    pub framing_shadow_y_offset: f64,

    pub line_below: bool,
    pub line_below_color: i32,
    pub line_below_width: f64,
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

/// A resolved part of a `CombinedSymbol`.
pub enum PartRef {
    None,
    /// Index into `Map::symbols`.
    Shared(usize),
    /// Index into `CombinedSymbol::owned_parts`.
    Private(usize),
}

/// A symbol which draws several other symbols on the same object.
#[derive(Default)]
pub struct CombinedSymbol {
    pub name: String,
    pub code: String,
    pub is_hidden: bool,
    pub is_helper_symbol: bool,
    pub is_rotatable: bool,

    /// The private parts of this symbol.
    pub owned_parts: Vec<Symbol>,
    /// Symbol id per part, -1 if private.
    pub part_ids: Vec<i32>,
    /// Resolved parts, may contain `PartRef::None`.
    pub parts: Vec<PartRef>,
}

/// A symbol describes how objects using it are drawn. This enum replaces
/// C++'s `Symbol` base class + virtual/`static_cast` dispatch; since Rust
/// has no inheritance, the fields the C++ base class carried (`name`,
/// `code`, `is_hidden`, `is_helper_symbol`, `is_rotatable`) are duplicated
/// directly on each variant's struct instead of a wrapper, so that e.g. a
/// `PointSymbol` nested inside a `LineSymbol` (its start/mid/end/dash
/// symbol) still carries its own `is_rotatable` -- exactly as the C++
/// `PointSymbol : public Symbol` inheritance does.
pub enum Symbol {
    Point(PointSymbol),
    Line(LineSymbol),
    Area(AreaSymbol),
    Text(TextSymbol),
    Combined(CombinedSymbol),
}

impl Symbol {
    pub fn is_hidden(&self) -> bool {
        match self {
            Symbol::Point(s) => s.is_hidden,
            Symbol::Line(s) => s.is_hidden,
            Symbol::Area(s) => s.is_hidden,
            Symbol::Text(s) => s.is_hidden,
            Symbol::Combined(s) => s.is_hidden,
        }
    }

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
    pub const LEFT: i32 = 0;
    pub const HCENTER: i32 = 1;
    pub const RIGHT: i32 = 2;
}

/// Vertical alignment values, as stored in the file format.
pub mod v_align {
    pub const BASELINE: i32 = 0;
    pub const TOP: i32 = 1;
    pub const VCENTER: i32 = 2;
    pub const BOTTOM: i32 = 3;
}

/// A path or area object's extra data.
#[derive(Default)]
pub struct PathObject {
    pub pattern_rotation: f64,
    pub pattern_origin: Point,
}

/// A text object's extra data.
pub struct TextObject {
    pub text: String,
    pub h_align: i32,
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

/// The variant data of an object.
pub enum ObjectKind {
    Point,
    Path(PathObject),
    Text(TextObject),
}

/// The base class of all map objects: coordinates plus a symbol.
pub struct Object {
    pub kind: ObjectKind,
    /// Symbol id from the file, -1 if embedded.
    pub symbol_id: i32,
    /// Resolved symbol, `None` if unresolved. Only meaningful for objects
    /// owned directly by `Map::objects`: a `PointSymbol::Element`'s symbol is
    /// given directly by `Element::symbol` instead, exactly as in the
    /// original, where `Object::symbol` is read only by `Renderer::addObject`.
    pub symbol_index: Option<usize>,
    pub coords: CoordList,
    /// Rotation in radians, for rotatable symbols.
    pub rotation: f64,
}

impl Object {
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
    pub scale_denominator: i32,
    pub colors: Vec<Color>,
    pub symbols: Vec<Symbol>,
    /// The file id of each symbol, parallel to `symbols`.
    pub symbol_ids: Vec<i32>,
    pub objects: Vec<Object>,
}

impl Map {
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
    opacity: 1.0,
});
