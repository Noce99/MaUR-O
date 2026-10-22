//! Writes a map in the native XML format of OpenOrienteering Mapper.
//!
//! The counterpart of [`crate::xml_reader`], and a much smaller thing than
//! it: this project renders maps rather than edits them, so the only maps it
//! ever writes are ones it has just generated. There are two kinds. A grid of
//! test objects for one symbol of an existing map (see
//! [`crate::all_symbols`]) keeps the colours and the symbols as the source
//! file's own bytes (see [`Fragments`](crate::xml_reader::Fragments)), and
//! only the objects, which are new, are actually written here. A map built
//! from nothing — [`crate::osm_convert`] makes one out of OpenStreetMap data —
//! has no source file to copy from, so [`color_element`] and
//! [`symbol_element`] write its colours and symbols out of the [`crate::map`]
//! model itself, and [`georeferencing_element`] says where on the earth it
//! sits.
//!
//! The document around them is the one Mapper writes: same element order,
//! same `barrier`, same defaults. That is what makes a generated map usable
//! as a benchmark map at all — it is opened by Mapper, and drawn by the
//! ground truth renderer, as one of their own files, so the reference image
//! it produces is a fair thing to be measured against.

use std::io::Write;
use std::path::Path;

use crate::map::{
    Border, Color, Coord, FillPattern, LineSymbol, Object, ObjectKind, PointSymbol, Symbol,
};
use crate::xml_reader::Fragment;

/// The format version this writer produces, as Mapper labels it.
const FORMAT_VERSION: &str = "9";

/// A number, the way `QString::number(double)` writes one: `%g` with 6
/// significant digits, which is what a Mapper file holds for a rotation.
fn number(value: f64) -> String {
    qt_number(value, 6)
}

/// A number, the way `QString::number(double, 'g', significant)` writes one.
///
/// That is C's `%g` — fixed notation while the exponent fits, scientific
/// otherwise, trailing zeros dropped either way — with one difference which
/// changes digits in practice: where the first digit dropped is exactly a
/// five and nothing follows it, Qt rounds away from zero, and Rust's own
/// formatting rounds to even. 41.25 to three significant digits is `41.3`
/// here and `41.2` from `format!("{:.1}")`, and a generated map's labels say
/// the former because the tool this project reproduces said the former.
///
/// Public because the same function writes the numbers in a file and the
/// numbers in the text next to it: both are `QString::number`.
pub fn qt_number(value: f64, significant: usize) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let significant = significant.max(1);

    // The exact decimal digits of the value, which is what the rounding
    // below has to look at: whether a five is a tie or is followed by more
    // decides which way it goes, and only the exact expansion knows.
    let exact = format!("{:.*e}", significant + 20, value.abs());
    let (mantissa, exponent) = exact.split_once('e').expect("a scientific form");
    let mut exponent: i32 = exponent.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();

    if digits.len() > significant {
        let round_up = digits[significant] >= 5;
        digits.truncate(significant);
        if round_up {
            let mut at = significant;
            loop {
                if at == 0 {
                    // Every digit was a nine: 9.99 becomes 10.0.
                    digits.insert(0, 1);
                    digits.pop();
                    exponent += 1;
                    break;
                }
                at -= 1;
                if digits[at] < 9 {
                    digits[at] += 1;
                    break;
                }
                digits[at] = 0;
            }
        }
    }

    let digit = |d: &u8| char::from(b'0' + d);
    let mut text = String::new();
    if value < 0.0 {
        text.push('-');
    }

    if exponent < -4 || exponent >= significant as i32 {
        text.push(digit(&digits[0]));
        let rest: String = digits[1..].iter().map(digit).collect();
        let rest = rest.trim_end_matches('0');
        if !rest.is_empty() {
            text.push('.');
            text.push_str(rest);
        }
        text.push('e');
        text.push(if exponent < 0 { '-' } else { '+' });
        text.push_str(&format!("{:02}", exponent.abs()));
        return text;
    }

    if exponent >= 0 {
        let whole = (exponent + 1) as usize;
        for i in 0..whole {
            text.push(digits.get(i).map_or('0', digit));
        }
        let rest: String = digits.iter().skip(whole).map(digit).collect();
        let rest = rest.trim_end_matches('0');
        if !rest.is_empty() {
            text.push('.');
            text.push_str(rest);
        }
    } else {
        text.push_str("0.");
        for _ in 0..(-exponent - 1) {
            text.push('0');
        }
        let rest: String = digits.iter().map(digit).collect();
        text.push_str(rest.trim_end_matches('0'));
    }
    text
}

/// Escapes the five characters XML reserves.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// A coordinate in native units: 1/1000 mm on the paper, which is what the
/// file holds and what Mapper rounds to as soon as it makes one.
fn native(mm: f64) -> i64 {
    crate::geometry::qround(mm * 1000.0) as i64
}

/// One object, as `Object::save()` writes it.
fn write_object(out: &mut String, object: &Object, rotatable: bool) {
    let kind = match object.kind {
        ObjectKind::Point => 0,
        ObjectKind::Path(_) => 1,
        ObjectKind::Text(_) => 4,
    };
    out.push_str(&format!("<object type=\"{kind}\""));
    if object.symbol_id >= 0 {
        out.push_str(&format!(" symbol=\"{}\"", object.symbol_id));
    }
    // A rotation is kept only where the symbol can be rotated, and only when
    // there is one, exactly as Mapper decides it.
    if rotatable && object.rotation != 0.0 {
        out.push_str(&format!(" rotation=\"{}\"", number(object.rotation)));
    }
    if let ObjectKind::Text(text) = &object.kind {
        out.push_str(&format!(
            " h_align=\"{}\" v_align=\"{}\"",
            text.h_align, text.v_align
        ));
    }
    out.push('>');

    out.push_str(&format!("<coords count=\"{}\">", object.coords.len()));
    for coord in &object.coords {
        write_coord(out, coord);
    }
    out.push_str("</coords>");

    match &object.kind {
        ObjectKind::Path(path) => {
            out.push_str(&format!(
                "<pattern rotation=\"{}\">",
                number(path.pattern_rotation)
            ));
            out.push_str(&format!(
                "<coord x=\"{}\" y=\"{}\"/>",
                native(path.pattern_origin.x),
                native(path.pattern_origin.y)
            ));
            out.push_str("</pattern>");
        }
        ObjectKind::Text(text) => {
            out.push_str(&format!("<text>{}</text>", escape(&text.text)));
        }
        ObjectKind::Point => {}
    }
    out.push_str("</object>");
}

/// One coordinate of a path, in the compact form the file format uses:
/// `x y;`, with the flags in between when there are any.
fn write_coord(out: &mut String, coord: &Coord) {
    out.push_str(&format!("{} {}", native(coord.x), native(coord.y)));
    if coord.flags != 0 {
        out.push_str(&format!(" {}", coord.flags));
    }
    out.push(';');
}

/// A length in native units, as an attribute: the integer the file holds.
fn length(mm: f64) -> i64 {
    native(mm)
}

/// One `<color>` element, as Mapper writes a colour defined by its RGB
/// values: the CMYK on the element is derived from them (`method="rgb"`),
/// and no spot colour separations are given.
pub fn color_element(priority: usize, color: &Color) -> String {
    let (r, g, b) = color.rgb;
    let (c, m, y, k) = rgb_to_cmyk(color.rgb);
    format!(
        "<color priority=\"{priority}\" name=\"{}\" c=\"{}\" m=\"{}\" y=\"{}\" k=\"{}\" opacity=\"{}\">\
         <spotcolors/><cmyk method=\"rgb\"/>\
         <rgb method=\"custom\" r=\"{}\" g=\"{}\" b=\"{}\"/></color>",
        escape(&color.name),
        qt_number(c, 3),
        qt_number(m, 3),
        qt_number(y, 3),
        qt_number(k, 3),
        number(color.opacity),
        number(f64::from(r)),
        number(f64::from(g)),
        number(f64::from(b)),
    )
}

/// The inks of an RGB colour, black taken as far as it will go: what Mapper
/// derives a colour's CMYK from when it is defined by RGB.
fn rgb_to_cmyk((r, g, b): (f32, f32, f32)) -> (f64, f64, f64, f64) {
    let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    let inv = 1.0 - k;
    (
        (1.0 - r / inv).clamp(0.0, 1.0),
        (1.0 - g / inv).clamp(0.0, 1.0),
        (1.0 - b / inv).clamp(0.0, 1.0),
        k,
    )
}

/// A symbol's name and code, whichever kind it is.
fn name_and_code(symbol: &Symbol) -> (&str, &str) {
    match symbol {
        Symbol::Point(s) => (&s.name, &s.code),
        Symbol::Line(s) => (&s.name, &s.code),
        Symbol::Area(s) => (&s.name, &s.code),
        Symbol::Text(s) => (&s.name, &s.code),
        Symbol::Combined(s) => (&s.name, &s.code),
    }
}

/// One `<symbol>` element of the symbol table, with the file id objects and
/// combined symbols refer to it by.
///
/// Writes what [`crate::map`] holds of a symbol, which is what the renderer
/// reads: enough to draw it the same way after reading it back, not every
/// setting Mapper's own editor keeps. A combined symbol's private parts are
/// written inside it, its shared ones by id.
pub fn symbol_element(id: i32, symbol: &Symbol) -> String {
    let mut out = String::new();
    write_symbol(&mut out, Some(id), symbol);
    out
}

/// A symbol, with an id where it is in the symbol table and without one
/// where it is nested inside another (an element, a mid symbol, a pattern).
fn write_symbol(out: &mut String, id: Option<i32>, symbol: &Symbol) {
    let kind = match symbol {
        Symbol::Point(_) => crate::map::symbol_type::POINT,
        Symbol::Line(_) => crate::map::symbol_type::LINE,
        Symbol::Area(_) => crate::map::symbol_type::AREA,
        Symbol::Text(_) => crate::map::symbol_type::TEXT,
        Symbol::Combined(_) => crate::map::symbol_type::COMBINED,
    };
    out.push_str(&format!("<symbol type=\"{kind}\""));
    if let Some(id) = id {
        out.push_str(&format!(" id=\"{id}\""));
    }
    let (name, code) = name_and_code(symbol);
    out.push_str(&format!(
        " code=\"{}\" name=\"{}\"",
        escape(code),
        escape(name)
    ));
    if symbol.is_hidden() {
        out.push_str(" is_hidden=\"true\"");
    }
    if symbol.is_helper_symbol() {
        out.push_str(" is_helper_symbol=\"true\"");
    }
    out.push('>');

    match symbol {
        Symbol::Point(point) => write_point_symbol(out, point),
        Symbol::Line(line) => write_line_symbol(out, line),
        Symbol::Area(area) => {
            out.push_str(&format!(
                "<area_symbol inner_color=\"{}\" min_area=\"{}\"{} patterns=\"{}\">",
                area.color,
                area.minimum_area,
                if area.is_rotatable {
                    " rotatable=\"true\""
                } else {
                    ""
                },
                area.patterns.len()
            ));
            for pattern in &area.patterns {
                write_fill_pattern(out, pattern);
            }
            out.push_str("</area_symbol>");
        }
        Symbol::Text(text) => {
            out.push_str(&format!(
                "<text_symbol rotatable=\"{}\"><font family=\"{}\" size=\"{}\"{}{}{}/>",
                text.is_rotatable,
                escape(&text.font_family),
                length(text.font_size),
                if text.bold { " bold=\"true\"" } else { "" },
                if text.italic { " italic=\"true\"" } else { "" },
                if text.underline {
                    " underline=\"true\""
                } else {
                    ""
                },
            ));
            out.push_str(&format!(
                "<text color=\"{}\" line_spacing=\"{}\" paragraph_spacing=\"{}\" character_spacing=\"{}\" kerning=\"{}\"/>",
                text.color,
                number(text.line_spacing),
                length(text.paragraph_spacing),
                number(text.character_spacing),
                text.kerning
            ));
            if text.framing {
                out.push_str(&format!(
                    "<framing color=\"{}\" mode=\"{}\" line_half_width=\"{}\" shadow_x_offset=\"{}\" shadow_y_offset=\"{}\"/>",
                    text.framing_color,
                    text.framing_mode,
                    length(text.framing_line_half_width),
                    length(text.framing_shadow_x_offset),
                    length(text.framing_shadow_y_offset)
                ));
            }
            if text.line_below {
                out.push_str(&format!(
                    "<line_below color=\"{}\" width=\"{}\" distance=\"{}\"/>",
                    text.line_below_color,
                    length(text.line_below_width),
                    length(text.line_below_distance)
                ));
            }
            out.push_str("</text_symbol>");
        }
        Symbol::Combined(combined) => {
            out.push_str(&format!(
                "<combined_symbol parts=\"{}\">",
                combined.part_ids.len()
            ));
            let mut private = combined.owned_parts.iter();
            for &part_id in &combined.part_ids {
                if part_id >= 0 {
                    out.push_str(&format!("<part symbol=\"{part_id}\"/>"));
                } else if let Some(part) = private.next() {
                    out.push_str("<part private=\"true\">");
                    write_symbol(out, None, part);
                    out.push_str("</part>");
                }
            }
            out.push_str("</combined_symbol>");
        }
    }
    out.push_str("</symbol>");
}

fn write_point_symbol(out: &mut String, point: &PointSymbol) {
    out.push_str(&format!(
        "<point_symbol{} inner_radius=\"{}\" inner_color=\"{}\" outer_width=\"{}\" outer_color=\"{}\" elements=\"{}\">",
        if point.is_rotatable { " rotatable=\"true\"" } else { "" },
        length(point.inner_radius),
        point.inner_color,
        length(point.outer_width),
        point.outer_color,
        point.elements.len()
    ));
    for element in &point.elements {
        out.push_str("<element>");
        write_symbol(out, None, &element.symbol);
        write_object(out, &element.object, false);
        out.push_str("</element>");
    }
    out.push_str("</point_symbol>");
}

fn write_line_symbol(out: &mut String, line: &LineSymbol) {
    out.push_str(&format!(
        "<line_symbol color=\"{}\" line_width=\"{}\" minimum_length=\"{}\" join_style=\"{}\" cap_style=\"{}\" start_offset=\"{}\" end_offset=\"{}\"",
        line.color,
        length(line.line_width),
        length(line.minimum_length),
        line.join_style,
        line.cap_style,
        length(line.start_offset),
        length(line.end_offset),
    ));
    if line.dashed {
        out.push_str(" dashed=\"true\"");
    }
    out.push_str(&format!(
        " segment_length=\"{}\" end_length=\"{}\" show_at_least_one_symbol=\"{}\" dash_length=\"{}\" break_length=\"{}\" dashes_in_group=\"{}\" in_group_break_length=\"{}\" mid_symbols_per_spot=\"{}\" mid_symbol_distance=\"{}\" mid_symbol_placement=\"{}\"",
        length(line.segment_length),
        length(line.end_length),
        line.show_at_least_one_symbol,
        length(line.dash_length),
        length(line.break_length),
        line.dashes_in_group,
        length(line.in_group_break_length),
        line.mid_symbols_per_spot,
        length(line.mid_symbol_distance),
        line.mid_symbol_placement,
    ));
    if line.half_outer_dashes {
        out.push_str(" half_outer_dashes=\"true\"");
    }
    if line.suppress_dash_symbol_at_ends {
        out.push_str(" suppress_dash_symbol_at_ends=\"true\"");
    }
    if !line.scale_dash_symbol {
        out.push_str(" scale_dash_symbol=\"false\"");
    }
    out.push('>');

    for (name, nested) in [
        ("start_symbol", &line.start_symbol),
        ("mid_symbol", &line.mid_symbol),
        ("end_symbol", &line.end_symbol),
        ("dash_symbol", &line.dash_symbol),
    ] {
        if let Some(point) = nested {
            out.push_str(&format!("<{name}><symbol type=\"1\" code=\"\">"));
            write_point_symbol(out, point);
            out.push_str(&format!("</symbol></{name}>"));
        }
    }

    if line.border.is_visible() || line.right_border.is_visible() {
        out.push_str("<borders>");
        write_border(out, &line.border);
        write_border(out, &line.right_border);
        out.push_str("</borders>");
    }
    out.push_str("</line_symbol>");
}

fn write_border(out: &mut String, border: &Border) {
    out.push_str(&format!(
        "<border color=\"{}\" width=\"{}\" shift=\"{}\"",
        border.color,
        length(border.width),
        length(border.shift)
    ));
    if border.dashed {
        out.push_str(&format!(
            " dashed=\"true\" dash_length=\"{}\" break_length=\"{}\"",
            length(border.dash_length),
            length(border.break_length)
        ));
    }
    out.push_str("/>");
}

fn write_fill_pattern(out: &mut String, pattern: &FillPattern) {
    out.push_str(&format!(
        "<pattern type=\"{}\" angle=\"{}\" no_clipping=\"{}\"{} line_spacing=\"{}\" line_offset=\"{}\" offset_along_line=\"{}\"",
        pattern.pattern_type,
        number(pattern.angle),
        pattern.no_clipping,
        if pattern.rotatable { " rotatable=\"true\"" } else { "" },
        length(pattern.line_spacing),
        length(pattern.line_offset),
        length(pattern.offset_along_line),
    ));
    if pattern.pattern_type == crate::map::fill_pattern_type::POINT {
        out.push_str(&format!(
            " point_distance=\"{}\">",
            length(pattern.point_distance)
        ));
        if let Some(point) = &pattern.point {
            out.push_str("<symbol type=\"1\" code=\"\">");
            write_point_symbol(out, point);
            out.push_str("</symbol>");
        }
    } else {
        out.push_str(&format!(
            " color=\"{}\" line_width=\"{}\">",
            pattern.line_color,
            length(pattern.line_width)
        ));
    }
    out.push_str("</pattern>");
}

/// Where a map sits on the earth: what [`georeferencing_element`] writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Georeference {
    /// The map scale: 6085 for a 1:6085 map.
    pub scale: i32,
    /// The EPSG code of the projected CRS.
    pub epsg: i32,
    /// The point at the map's own origin, as `(easting, northing)` in the
    /// projected CRS.
    pub ref_point: (f64, f64),
    /// The same point, as `(lat, lon)` in degrees, which Mapper keeps
    /// alongside so a map can be placed again without a projection library.
    pub ref_lat_lon: (f64, f64),
    /// The angle between grid north and map north, in degrees.
    pub grivation: f64,
    /// The angle between true north and magnetic north, in degrees.
    pub declination: f64,
    /// The projection's own scale at the reference point.
    pub grid_scale_factor: f64,
}

/// The `<georeferencing>` element of a map placed in a projected CRS named
/// by its EPSG code, as Mapper writes one.
pub fn georeferencing_element(georef: &Georeference) -> String {
    format!(
        "<georeferencing scale=\"{}\" grid_scale_factor=\"{}\" auxiliary_scale_factor=\"1\" declination=\"{}\" grivation=\"{}\">\
         <ref_point x=\"0\" y=\"0\"/>\
         <projected_crs id=\"EPSG\"><spec language=\"PROJ.4\">+init=epsg:{}</spec><parameter>{}</parameter>\
         <ref_point x=\"{:.6}\" y=\"{:.6}\"/></projected_crs>\
         <geographic_crs id=\"Geographic coordinates\"><spec language=\"PROJ.4\">+proj=latlong +datum=WGS84</spec>\
         <ref_point_deg lat=\"{:.8}\" lon=\"{:.8}\"/></geographic_crs></georeferencing>",
        georef.scale,
        number(georef.grid_scale_factor),
        qt_number(georef.declination, 4),
        qt_number(georef.grivation, 4),
        georef.epsg,
        georef.epsg,
        georef.ref_point.0,
        georef.ref_point.1,
        georef.ref_lat_lon.0,
        georef.ref_lat_lon.1,
    )
}

/// A map file to be written: what it holds, and what its objects are drawn
/// with.
pub struct MapFile<'a> {
    /// The scale the objects' coordinates are to be read at.
    pub scale_denominator: i32,
    /// The colours, as the file they came from spells them. Their order is
    /// the drawing order of the whole map, and a symbol refers to one of them
    /// by its position here, so they go out whole and in order.
    pub colors: &'a [Fragment],
    /// The symbols, likewise. An object refers to one of them by the `id`
    /// attribute the fragment carries, which is why the ids are left as the
    /// source had them rather than renumbered.
    pub symbols: &'a [Fragment],
    /// The objects, in drawing order.
    pub objects: &'a [Object],
    /// Whether an object's rotation is worth writing down, i.e. whether the
    /// symbol it is drawn with is rotatable.
    pub rotatable: bool,
    /// The `<georeferencing>` element, as [`georeferencing_element`] writes
    /// one, or `None` for a map placed on no ground at all: local
    /// coordinates, at the scale above.
    pub georeferencing: Option<String>,
}

impl MapFile<'_> {
    /// The whole document, as text.
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str(&format!(
            "<map xmlns=\"http://openorienteering.org/apps/mapper/xml/v2\" version=\"{FORMAT_VERSION}\">\n"
        ));
        out.push_str("<notes></notes>\n");
        match &self.georeferencing {
            Some(element) => {
                out.push_str(element);
                out.push('\n');
            }
            None => out.push_str(&format!(
                "<georeferencing scale=\"{}\"><projected_crs id=\"Local\"/></georeferencing>\n",
                self.scale_denominator
            )),
        }

        out.push_str(&format!("<colors count=\"{}\">\n", self.colors.len()));
        for color in self.colors {
            out.push_str(&color.text);
            out.push('\n');
        }
        out.push_str("</colors>\n");

        // Everything a reader older than the barrier's version cannot make
        // sense of goes inside it, which is where Mapper puts the symbols.
        out.push_str("<barrier version=\"6\" required=\"0.6.0\">\n");
        out.push_str(&format!("<symbols count=\"{}\">\n", self.symbols.len()));
        for symbol in self.symbols {
            out.push_str(&symbol.text);
            out.push('\n');
        }
        out.push_str("</symbols>\n");

        out.push_str("<parts count=\"1\" current=\"0\">\n");
        out.push_str(&format!(
            "<part name=\"default part\"><objects count=\"{}\">\n",
            self.objects.len()
        ));
        for object in self.objects {
            write_object(&mut out, object, self.rotatable);
            out.push('\n');
        }
        out.push_str("</objects></part>\n");
        out.push_str("</parts>\n");

        out.push_str("<templates count=\"0\" first_front_template=\"0\">\n");
        out.push_str("<defaults use_meters_per_pixel=\"true\" meters_per_pixel=\"0\" dpi=\"0\" scale=\"0\"/>\n");
        out.push_str("</templates>\n");
        out.push_str("</barrier>\n");
        out.push_str("</map>\n");
        out
    }

    /// Writes the document to a file.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let file = std::fs::File::create(path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let mut out = std::io::BufWriter::new(file);
        out.write_all(self.to_xml().as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{Point, TextObject};

    #[test]
    fn a_rotation_is_written_the_way_qt_writes_a_double() {
        // The values of the eight rotation steps, as a Mapper file holds them.
        assert_eq!(number(0.0), "0");
        assert_eq!(number(std::f64::consts::PI / 4.0), "0.785398");
        assert_eq!(number(std::f64::consts::PI / 2.0), "1.5708");
        assert_eq!(number(std::f64::consts::PI), "3.14159");
        assert_eq!(number(7.0 * std::f64::consts::PI / 4.0), "5.49779");
    }

    #[test]
    fn a_number_keeps_the_zeros_which_are_not_decimals() {
        // Trailing zeros go from the decimals and stay in the whole part,
        // which "900" to three significant digits is all of.
        assert_eq!(qt_number(900.0, 3), "900");
        assert_eq!(qt_number(100.0, 6), "100");
        assert_eq!(qt_number(50.0, 6), "50");
        assert_eq!(qt_number(150.0, 6), "150");
    }

    #[test]
    fn a_tie_rounds_away_from_zero_as_qt_rounds_it() {
        // Rust's own formatting rounds these to even: 41.2 and 0.5.
        assert_eq!(qt_number(41.25, 3), "41.3");
        assert_eq!(qt_number(0.125, 2), "0.13");
        assert_eq!(qt_number(-41.25, 3), "-41.3");
        // Not a tie: the exact value of 2.675 is below the half.
        assert_eq!(qt_number(2.675, 3), "2.67");
    }

    #[test]
    fn a_carry_across_every_digit_moves_the_point() {
        assert_eq!(qt_number(9.999, 3), "10");
        assert_eq!(qt_number(0.9999, 3), "1");
        assert_eq!(qt_number(99.96, 3), "100");
    }

    #[test]
    fn a_number_too_large_or_too_small_for_the_digits_goes_scientific() {
        // %g switches at an exponent of -5 and at the significant digits.
        assert_eq!(qt_number(9000.0, 3), "9e+03");
        assert_eq!(qt_number(0.0001, 3), "0.0001");
        assert_eq!(qt_number(0.00001234, 3), "1.23e-05");
        assert_eq!(qt_number(1234567.0, 6), "1.23457e+06");
    }

    #[test]
    fn small_numbers_keep_their_leading_zeros() {
        assert_eq!(qt_number(0.0234567, 3), "0.0235");
        assert_eq!(qt_number(0.5, 3), "0.5");
    }

    #[test]
    fn a_path_object_carries_its_coordinates_in_native_units() {
        let mut object = Object::new(ObjectKind::Path(Default::default()));
        object.symbol_id = 3;
        object.coords = vec![Coord::new(0.105, 0.105, 0), Coord::new(0.605, 0.105, 2)];
        let mut out = String::new();
        write_object(&mut out, &object, false);
        assert_eq!(
            out,
            "<object type=\"1\" symbol=\"3\"><coords count=\"2\">105 105;605 105 2;</coords>\
             <pattern rotation=\"0\"><coord x=\"0\" y=\"0\"/></pattern></object>"
        );
    }

    #[test]
    fn a_text_object_carries_its_alignment_and_its_text() {
        let mut object = Object::new(ObjectKind::Text(TextObject {
            text: "Sample 123".to_string(),
            h_align: 1,
            v_align: 2,
            box_size: None,
        }));
        object.symbol_id = 0;
        object.rotation = std::f64::consts::PI / 4.0;
        object.coords = vec![Coord::new(8.416, 0.707, 0)];
        let mut out = String::new();
        write_object(&mut out, &object, true);
        assert_eq!(
            out,
            "<object type=\"4\" symbol=\"0\" rotation=\"0.785398\" h_align=\"1\" v_align=\"2\">\
             <coords count=\"1\">8416 707;</coords><text>Sample 123</text></object>"
        );
    }

    #[test]
    fn a_rotation_is_left_out_where_the_symbol_cannot_be_rotated() {
        let mut object = Object::new(ObjectKind::Point);
        object.symbol_id = 0;
        object.rotation = 1.0;
        object.coords = vec![Coord::new(0.0, 0.0, 0)];
        let mut out = String::new();
        write_object(&mut out, &object, false);
        assert!(!out.contains("rotation"), "{out}");
    }

    #[test]
    fn text_is_escaped() {
        let mut object = Object::new(ObjectKind::Text(TextObject {
            text: "a < b & \"c\"".to_string(),
            ..Default::default()
        }));
        object.coords = vec![Coord::new(0.0, 0.0, 0)];
        let mut out = String::new();
        write_object(&mut out, &object, false);
        assert!(
            out.contains("<text>a &lt; b &amp; &quot;c&quot;</text>"),
            "{out}"
        );
    }

    #[test]
    fn written_colours_and_symbols_read_back_the_same() {
        use crate::map::{
            fill_pattern_type, AreaSymbol, CombinedSymbol, LineSymbol, PartRef, PointSymbol,
        };
        let color = Color {
            name: "Road & casing".to_string(),
            rgb: (1.0, 0.5, 0.25),
            opacity: 0.4,
        };
        let line = LineSymbol {
            name: "dashed".to_string(),
            color: 0,
            line_width: 0.364,
            cap_style: crate::map::cap_style::ROUND,
            join_style: crate::map::join_style::ROUND,
            dashed: true,
            dash_length: 0.84,
            break_length: 0.98,
            border: Border {
                color: 0,
                width: 0.1,
                shift: 0.05,
                ..Border::default()
            },
            right_border: Border {
                color: 0,
                width: 0.1,
                shift: 0.05,
                ..Border::default()
            },
            ..LineSymbol::default()
        };
        let mut area = AreaSymbol {
            name: "hatched".to_string(),
            color: 0,
            ..AreaSymbol::new()
        };
        area.patterns.push(FillPattern {
            pattern_type: fill_pattern_type::LINE,
            angle: std::f64::consts::FRAC_PI_4,
            line_spacing: 1.584,
            line_color: 0,
            line_width: 0.198,
            ..FillPattern::default()
        });
        let point = PointSymbol {
            name: "dot".to_string(),
            inner_radius: 0.35,
            inner_color: 0,
            ..PointSymbol::new()
        };
        let combined = Symbol::Combined(CombinedSymbol {
            name: "road".to_string(),
            part_ids: vec![-1, 1],
            owned_parts: vec![Symbol::Line(line)],
            ..CombinedSymbol::default()
        });

        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <map xmlns=\"http://openorienteering.org/apps/mapper/xml/v2\" version=\"9\">\
             <georeferencing scale=\"6085\"/><colors count=\"1\">",
        );
        xml.push_str(&color_element(0, &color));
        xml.push_str("</colors><barrier version=\"6\" required=\"0.6.0\"><symbols count=\"4\">");
        xml.push_str(&symbol_element(0, &combined));
        xml.push_str(&symbol_element(1, &Symbol::Area(area)));
        xml.push_str(&symbol_element(2, &Symbol::Point(point)));
        xml.push_str("</symbols><parts count=\"0\"/></barrier></map>");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("written.omap");
        std::fs::write(&path, xml).unwrap();
        let (map, _) = crate::xml_reader::read_xml_map(&path).unwrap();

        let read_color = &map.colors[0];
        assert_eq!(read_color.name, "Road & casing");
        assert_eq!(read_color.rgb, (1.0, 0.5, 0.25));
        assert_eq!(read_color.opacity, 0.4);

        let Symbol::Combined(read_combined) = &map.symbols[0] else {
            panic!("not combined");
        };
        assert!(matches!(
            read_combined.parts[..],
            [PartRef::Private(0), PartRef::Shared(1)]
        ));
        let Symbol::Line(read_line) = &read_combined.owned_parts[0] else {
            panic!("not a line");
        };
        assert!(read_line.dashed);
        assert_eq!(read_line.line_width, 0.364);
        assert_eq!(
            (read_line.dash_length, read_line.break_length),
            (0.84, 0.98)
        );
        assert_eq!(read_line.cap_style, crate::map::cap_style::ROUND);
        assert!(read_line.border.is_visible() && read_line.right_border.is_visible());
        assert_eq!(read_line.right_border.shift, 0.05);

        let Symbol::Area(read_area) = &map.symbols[1] else {
            panic!("not an area");
        };
        let pattern = &read_area.patterns[0];
        assert_eq!(pattern.line_spacing, 1.584);
        assert_eq!(pattern.line_width, 0.198);
        assert!((pattern.angle - std::f64::consts::FRAC_PI_4).abs() < 1e-6);

        let Symbol::Point(read_point) = &map.symbols[2] else {
            panic!("not a point");
        };
        // Native units are integers: what comes back is 350 thousandths.
        assert!((read_point.inner_radius - 0.35).abs() < 1e-12);
        assert_eq!(read_point.inner_color, 0);
    }

    #[test]
    fn a_georeferencing_element_names_its_crs_and_reference_point() {
        let element = georeferencing_element(&Georeference {
            scale: 6085,
            epsg: 32632,
            ref_point: (686_512.25, 4_929_301.5),
            ref_lat_lon: (44.4938, 11.343),
            grivation: -1.64,
            declination: 0.0,
            grid_scale_factor: 1.00002,
        });
        for expected in [
            "scale=\"6085\"",
            "declination=\"0\" grivation=\"-1.64\"",
            "+init=epsg:32632</spec><parameter>32632</parameter>",
            "<ref_point x=\"686512.250000\" y=\"4929301.500000\"/>",
            "<ref_point_deg lat=\"44.49380000\" lon=\"11.34300000\"/>",
        ] {
            assert!(element.contains(expected), "{expected} not in {element}");
        }
    }

    #[test]
    fn a_pattern_origin_is_written_in_native_units() {
        let mut object = Object::new(ObjectKind::Path(crate::map::PathObject {
            pattern_rotation: std::f64::consts::PI / 2.0,
            pattern_origin: Point::new(1.5, -2.25),
        }));
        object.coords = vec![Coord::new(0.0, 0.0, 0)];
        let mut out = String::new();
        write_object(&mut out, &object, true);
        assert!(
            out.contains("<pattern rotation=\"1.5708\"><coord x=\"1500\" y=\"-2250\"/></pattern>"),
            "{out}"
        );
    }
}
