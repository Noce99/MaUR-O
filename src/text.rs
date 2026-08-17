//! Laying out and drawing the text objects of a map: labels, control
//! descriptions, whatever the mapper typed onto the paper.
//!
//! The text is laid out at a large font size (Mapper's own
//! `internal_font_size`) and scaled down afterwards, so the result does not
//! depend on hinting. Family names are resolved through the system's
//! fontconfig library (the same one Qt calls into on Linux), then handed to
//! `fontdb` to actually load the matched font file: `fontdb`'s own bundled
//! fontconfig-parser only reimplements enough of fontconfig's config-file
//! format to enumerate installed fonts, not its full alias/substitution
//! algorithm, so it can resolve a generic family like "Sans Serif" to a
//! different concrete font than Qt does on the same machine. Shaping goes
//! through `rustybuzz`, a pure-Rust port of HarfBuzz -- the same shaping
//! engine family Qt5 uses by default, which should make kerning and
//! advances closer to Qt's own than a naive per-glyph-advance layout would
//! be. Glyph outlines come from `ttf-parser`.
//!
//! This is the module most likely to disagree with the ground truth, and the
//! one place where that is expected rather than a bug. Exact glyph metrics
//! belong to whichever font engine produced them, and matching Qt and
//! FreeType outline for outline is not something a different stack can
//! promise the way it can promise a straight line. What is asked of the text
//! here is that it land in the right place, at the right size, with the right
//! alignment — so a benchmark difference on a label is worth reading as
//! "which of those is off?" before reading it as a rendering fault.

use std::sync::OnceLock;

use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use tiny_skia::Transform;

use crate::geometry::{Path, PathCommand, PenCap, PenJoin, Rect};
use crate::map::*;
use crate::renderer::Renderer;

/// The font size used while laying out text, before scaling it down to mm.
const INTERNAL_FONT_SIZE: f64 = 256.0;

fn font_db() -> &'static Database {
    static DB: OnceLock<Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        db
    })
}

fn family_for(name: &str) -> Family<'_> {
    // Mapper stores Qt's own generic family names ("Sans Serif", "Serif",
    // "Monospace", ...), not the CSS hyphenated spellings; recognize both.
    match name.to_ascii_lowercase().as_str() {
        "serif" => Family::Serif,
        "sans-serif" | "sans serif" => Family::SansSerif,
        "monospace" => Family::Monospace,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        _ => Family::Name(name),
    }
}

/// Resolves `name` (a font family, possibly one of Qt's generic names) to
/// the concrete family fontconfig would substitute it with for the given
/// style, the same way Qt's `QFontEngine` does under the hood. Returns
/// `None` if fontconfig is unavailable or the lookup fails, in which case
/// callers fall back to `fontdb`'s own (less faithful) generic-family
/// resolution.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn fontconfig_family_for(name: &str, bold: bool, italic: bool) -> Option<String> {
    use std::ffi::CString;

    static FC: OnceLock<Option<fontconfig::Fontconfig>> = OnceLock::new();
    let fc = FC.get_or_init(fontconfig::Fontconfig::new).as_ref()?;

    // fontconfig expects the CSS/hyphenated spelling of the generic names.
    let pattern_family = match name.to_ascii_lowercase().as_str() {
        "serif" => "serif",
        "sans-serif" | "sans serif" => "sans-serif",
        "monospace" => "monospace",
        "cursive" => "cursive",
        "fantasy" => "fantasy",
        _ => name,
    };
    let style = match (bold, italic) {
        (true, true) => Some("Bold Italic"),
        (true, false) => Some("Bold"),
        (false, true) => Some("Italic"),
        (false, false) => None,
    };

    let mut pattern = fontconfig::Pattern::new(fc).ok()?;
    pattern.add_string(fontconfig::FC_FAMILY, &CString::new(pattern_family).ok()?).ok()?;
    if let Some(style) = style {
        pattern.add_string(fontconfig::FC_STYLE, &CString::new(style).ok()?).ok()?;
    }
    let matched = pattern.font_match().ok()?;
    matched.get_string(fontconfig::FC_FAMILY).ok().map(str::to_owned)
}

#[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android")))))]
fn fontconfig_family_for(_name: &str, _bold: bool, _italic: bool) -> Option<String> {
    None
}

fn map_point(t: &Transform, p: Point) -> Point {
    let mut sp = tiny_skia::Point { x: p.x as f32, y: p.y as f32 };
    t.map_point(&mut sp);
    Point::new(sp.x as f64, sp.y as f64)
}

/// The bounding box of a rectangle mapped through a general (possibly
/// rotated) transform, like `QTransform::mapRect(QRectF)`.
fn map_rect(t: &Transform, r: &Rect) -> Rect {
    let corners = [
        Point::new(r.left(), r.top()),
        Point::new(r.right(), r.top()),
        Point::new(r.right(), r.bottom()),
        Point::new(r.left(), r.bottom()),
    ];
    let mapped: Vec<Point> = corners.iter().map(|&p| map_point(t, p)).collect();
    let xmin = mapped.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let xmax = mapped.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    let ymin = mapped.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let ymax = mapped.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
    Rect::from_ltrb(xmin, ymin, xmax, ymax)
}

fn map_path(t: &Transform, path: &Path) -> Path {
    let mut out = Path::new();
    for cmd in &path.commands {
        match *cmd {
            PathCommand::MoveTo(p) => out.move_to(map_point(t, p)),
            PathCommand::LineTo(p) => out.line_to(map_point(t, p)),
            PathCommand::CubicTo(c1, c2, end) => {
                out.cubic_to(map_point(t, c1), map_point(t, c2), map_point(t, end))
            }
            PathCommand::Close => out.close_subpath(),
        }
    }
    out
}

/// Converts a glyph's outline (font design units, y pointing up) into our
/// path IR (paper coordinates, y pointing down), positioned at `origin` in
/// `INTERNAL_FONT_SIZE`-scaled units. Quadratic segments (TrueType glyphs)
/// are degree-elevated to cubic, since our `Path` only has `cubic_to`.
struct GlyphOutlineBuilder {
    path: Path,
    scale: f64,
    origin: Point,
    current: Point,
}

impl GlyphOutlineBuilder {
    fn map(&self, x: f32, y: f32) -> Point {
        Point::new(self.origin.x + x as f64 * self.scale, self.origin.y - y as f64 * self.scale)
    }
}

impl rustybuzz::ttf_parser::OutlineBuilder for GlyphOutlineBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.path.move_to(p);
        self.current = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.path.line_to(p);
        self.current = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let q = self.map(x1, y1);
        let end = self.map(x, y);
        let c1 = self.current + (q - self.current) * (2.0 / 3.0);
        let c2 = end + (q - end) * (2.0 / 3.0);
        self.path.cubic_to(c1, c2, end);
        self.current = end;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c1 = self.map(x1, y1);
        let c2 = self.map(x2, y2);
        let end = self.map(x, y);
        self.path.cubic_to(c1, c2, end);
        self.current = end;
    }
    fn close(&mut self) {
        self.path.close_subpath();
    }
}

/// Shapes one line of text, returning its total advance width (in
/// `INTERNAL_FONT_SIZE` units, including `letter_spacing` after every
/// glyph) and each glyph's id and advance.
fn shape_line(face: &rustybuzz::Face, text: &str, kerning: bool, letter_spacing: f64) -> (f64, Vec<(u32, f64)>) {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(rustybuzz::Direction::LeftToRight);

    let features: Vec<rustybuzz::Feature> = if !kerning {
        vec![rustybuzz::Feature::new(rustybuzz::ttf_parser::Tag::from_bytes(b"kern"), 0, ..)]
    } else {
        Vec::new()
    };

    let units_per_em = face.units_per_em() as f64;
    let font_scale = if units_per_em > 0.0 { INTERNAL_FONT_SIZE / units_per_em } else { 1.0 };

    let glyph_buffer = rustybuzz::shape(face, &features, buffer);
    let infos = glyph_buffer.glyph_infos();
    let positions = glyph_buffer.glyph_positions();

    let mut width = 0.0;
    let mut glyphs = Vec::with_capacity(infos.len());
    for (info, pos) in infos.iter().zip(positions.iter()) {
        let advance = pos.x_advance as f64 * font_scale;
        glyphs.push((info.glyph_id, advance));
        width += advance + letter_spacing;
    }
    (width, glyphs)
}

struct TextLayout {
    /// In `INTERNAL_FONT_SIZE` units, anchored at the object's origin.
    text_path: Path,
    line_below_path: Path,
}

fn build_text_layout(data: &[u8], face_index: u32, symbol: &TextSymbol, text_object: &TextObject) -> Option<TextLayout> {
    let face = rustybuzz::Face::from_slice(data, face_index)?;
    let units_per_em = face.units_per_em() as f64;
    if units_per_em <= 0.0 { return None; }
    let font_scale = INTERNAL_FONT_SIZE / units_per_em;

    let ascender = face.ascender() as f64 * font_scale;
    let descender = face.descender() as f64 * font_scale;
    let line_gap = face.line_gap() as f64 * font_scale;
    // Approximates QFontMetricsF::lineSpacing() (leading + ascent + descent).
    let natural_line_spacing = ascender - descender + line_gap;

    // Letter spacing is measured from a space glyph's raw advance, mirroring
    // Mapper's `font.setLetterSpacing(AbsoluteSpacing, plain.horizontalAdvance(" ") * character_spacing)`.
    let (space_advance, _) = shape_line(&face, " ", true, 0.0);
    let letter_spacing = space_advance * symbol.character_spacing;

    let line_spacing = symbol.line_spacing * natural_line_spacing;
    let inverse_scale = INTERNAL_FONT_SIZE / symbol.font_size;
    let mut paragraph_spacing = symbol.paragraph_spacing * inverse_scale;
    if symbol.line_below {
        paragraph_spacing += (symbol.line_below_distance + symbol.line_below_width) * inverse_scale;
    }

    // A box (two coordinates, one the anchor and one its size) does not
    // just word-wrap -- unimplemented here, since none of this renderer's
    // reference maps carry a box text long enough to need it -- it also
    // moves a `Top` or `Baseline`-anchored line up, and a `Bottom`-anchored
    // one down, by half the box height, and shifts a `Left`- or
    // `Right`-anchored line sideways by half the box width. `HCenter`,
    // already centered on the text's own width, is unaffected.
    let (box_width, box_height) = match text_object.box_size {
        Some((w, h)) => (inverse_scale * w, inverse_scale * h),
        None => (0.0, 0.0),
    };
    let has_box = text_object.box_size.is_some();

    let lines: Vec<&str> = text_object.text.split('\n').collect();
    let num_lines = lines.len();
    let height = ascender + (num_lines as f64 - 1.0) * (line_spacing + paragraph_spacing);

    let delta_y = match text_object.v_align {
        v_align::VCENTER => -0.5 * height,
        v_align::BOTTOM => -height + 0.5 * box_height,
        _ => 0.0,
    };
    let box_offset_y = if has_box && matches!(text_object.v_align, v_align::TOP | v_align::BASELINE) {
        -0.5 * box_height
    } else {
        0.0
    };

    let mut text_path = Path::new();
    let mut line_below_path = Path::new();
    let mut line_y = if text_object.v_align == v_align::BASELINE { 0.0 } else { ascender };
    line_y += box_offset_y + delta_y;

    for line in &lines {
        let (width, glyphs) = shape_line(&face, line, symbol.kerning, letter_spacing);
        let line_x = match text_object.h_align {
            h_align::HCENTER => -0.5 * width,
            h_align::RIGHT => 0.5 * box_width - width,
            _ => -0.5 * box_width,
        };

        if !line.is_empty() {
            let mut pen_x = line_x;
            for (glyph_id, advance) in &glyphs {
                let origin = Point::new(pen_x, line_y);
                let mut builder = GlyphOutlineBuilder { path: Path::new(), scale: font_scale, origin, current: Point::ZERO };
                face.outline_glyph(rustybuzz::ttf_parser::GlyphId(*glyph_id as u16), &mut builder);
                text_path.add_path(&builder.path);
                pen_x += advance + letter_spacing;
            }
        }

        if symbol.line_below && symbol.line_below_width > 0.0 {
            let y0 = line_y + symbol.line_below_distance * inverse_scale;
            let h = symbol.line_below_width * inverse_scale;
            line_below_path.move_to(Point::new(line_x, y0));
            line_below_path.line_to(Point::new(line_x + width, y0));
            line_below_path.line_to(Point::new(line_x + width, y0 + h));
            line_below_path.line_to(Point::new(line_x, y0 + h));
            line_below_path.close_subpath();
        }

        line_y += line_spacing + paragraph_spacing;
    }

    Some(TextLayout { text_path, line_below_path })
}

/// Lays out `text_object` and hands the resulting glyph outlines to
/// `renderer`: the text itself, its framing or shadow where the symbol asks
/// for one, and the line below it where it has one.
///
/// Nothing is added at all for an object with no anchor, empty text, a
/// non-positive font size, or a font family no face could be matched for.
///
/// The outlines go in at the internal font size, with the scale down to paper
/// millimetres carried on the renderable's transform instead of baked into
/// the path, so that curves are flattened at the size they are finally drawn
/// at rather than at the size they were laid out at.
pub fn add_text(renderer: &mut Renderer, symbol: &TextSymbol, object: &Object, text_object: &TextObject) {
    if object.coords.is_empty() || text_object.text.is_empty() || symbol.font_size <= 0.0 {
        return;
    }

    let db = font_db();
    let resolved = fontconfig_family_for(&symbol.font_family, symbol.bold, symbol.italic);
    let mut families = Vec::with_capacity(3);
    if let Some(name) = &resolved {
        families.push(Family::Name(name));
    }
    families.push(family_for(&symbol.font_family));
    families.push(Family::SansSerif);

    let query = Query {
        families: &families,
        weight: if symbol.bold { Weight::BOLD } else { Weight::NORMAL },
        style: if symbol.italic { Style::Italic } else { Style::Normal },
        stretch: Stretch::Normal,
    };
    let font_id = match db.query(&query) {
        Some(id) => id,
        None => return,
    };

    let layout = match db.with_face_data(font_id, |data, face_index| build_text_layout(data, face_index, symbol, text_object)) {
        Some(Some(l)) => l,
        _ => return,
    };

    let scale = symbol.font_size / INTERNAL_FONT_SIZE;
    let inverse_scale = 1.0 / scale;

    let origin = object.coords[0].pos();
    let mut transform = Transform::from_translate(origin.x as f32, origin.y as f32);
    transform = transform.pre_scale(scale as f32, scale as f32);
    // Whether the symbol says it is rotatable does not come into it: a text
    // object carries its own rotation, and `TextRenderable`'s constructor
    // applies it whenever there is one. (A point symbol is the other way
    // round, and `Renderer::add_symbol` follows it there.) So a text object
    // turned 45 degrees is drawn turned even where its symbol is not marked
    // rotatable — which also makes it that much wider, and a map laid out
    // around such an object that much wider again.
    if object.rotation != 0.0 {
        transform = transform.pre_rotate((-object.rotation).to_degrees() as f32);
    }

    // Mapper takes the extent of a text renderable from the control points
    // of the glyph outlines, and maps that rectangle rather than the
    // glyphs. The path itself stays in the units of the internal font size
    // and is drawn under the transform, so that curves are flattened at the
    // size at which they end up on the image.
    let text_extent = map_rect(&transform, &layout.text_path.control_point_rect());

    if symbol.framing && is_color(symbol.framing_color) {
        if symbol.framing_mode == 1 && symbol.framing_line_half_width > 0.0 {
            let fw = symbol.framing_line_half_width;
            let framing_extent = text_extent.adjusted(-fw, -fw, fw, fw);
            renderer.stroke(
                layout.text_path.clone(), symbol.framing_color,
                2.0 * fw * inverse_scale, PenCap::Round, PenJoin::Round,
                Some(framing_extent), Some(transform),
            );
        } else if symbol.framing_mode == 2 {
            let shadow = transform.pre_translate(
                (inverse_scale * symbol.framing_shadow_x_offset) as f32,
                (inverse_scale * symbol.framing_shadow_y_offset) as f32,
            );
            let shadow_extent = map_rect(&shadow, &layout.text_path.control_point_rect());
            renderer.fill_winding(layout.text_path.clone(), symbol.framing_color, Some(shadow_extent), Some(shadow));
        }
    }

    renderer.fill_winding(layout.text_path.clone(), symbol.color, Some(text_extent), Some(transform));

    if !layout.line_below_path.is_empty() {
        renderer.fill(map_path(&transform, &layout.line_below_path), symbol.line_below_color, None, None);
    }
}
