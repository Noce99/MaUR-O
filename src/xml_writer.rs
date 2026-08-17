//! Writes a map in the native XML format of OpenOrienteering Mapper.
//!
//! The counterpart of [`crate::xml_reader`], and a much smaller thing than
//! it: this project renders maps rather than edits them, so the only maps it
//! ever writes are ones it has just generated — a grid of test objects for
//! one symbol of an existing map, see [`crate::all_symbols`]. So the colours
//! and the symbols go out as the source file's own bytes (see
//! [`Fragments`](crate::xml_reader::Fragments)), and only the objects, which
//! are new, are actually written here.
//!
//! The document around them is the one Mapper writes: same element order,
//! same `barrier`, same defaults. That is what makes a generated map usable
//! as a benchmark map at all — it is opened by Mapper, and drawn by the
//! ground truth renderer, as one of their own files, so the reference image
//! it produces is a fair thing to be measured against.

use std::io::Write;
use std::path::Path;

use crate::map::{Coord, Object, ObjectKind};
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
            text.push(digits.get(i).map_or('0', &digit));
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
        out.push_str(&format!(
            "<georeferencing scale=\"{}\"><projected_crs id=\"Local\"/></georeferencing>\n",
            self.scale_denominator
        ));

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
