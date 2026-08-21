//! Reads an OCAD file, and writes the map out as Mapper's own XML.
//!
//! OCAD's `.ocd` is the other format an orienteering map comes in, and it is
//! a binary one: blocks of fixed-size records, chained by file offsets, whose
//! layout changed with almost every version of the program. This module reads
//! versions 6 through 12 and 2018, and converts what it finds into the XML
//! [`crate::xml_reader`] already parses.
//!
//! ```no_run
//! use maur_o::{ocd, xml_reader};
//!
//! let bytes = std::fs::read("map.ocd").expect("cannot read the file");
//! let imported = ocd::ocd_to_omap_xml(&bytes).expect("cannot import the file");
//! let (map, _) = xml_reader::read_xml_map_str(&imported.xml).expect("cannot parse the XML");
//! println!("1:{} from an OCAD {} file", map.scale_denominator, imported.version);
//! ```
//!
//! # Why XML in the middle
//!
//! Going through the XML rather than building a [`crate::map::Map`] directly
//! costs a serialization and a parse, and buys three things. Every map takes
//! one path into the model, so a symbol imported from OCAD is a symbol read
//! from a file and cannot drift from one; the result is a map text, which a
//! caller can save, embed or hand on; and the translation can be checked by
//! reading it, which is worth a great deal in a format documented mainly by
//! the two importers that came before this one.
//!
//! # Where the rules come from
//!
//! The binary layouts and the translation of one symbol system into the other
//! follow the two reference importers: OpenOrienteering Mapper's
//! `src/fileformats/ocd_*` above all, and Purple Pen's `MapModel`
//! `OcadImport.cs`. Where a choice looks arbitrary here, it is theirs, and
//! the comment says which of their functions it comes from.
//!
//! # Units
//!
//! OCAD lengths are 1/100 mm and Mapper's XML wants 1/1000 mm, so lengths are
//! multiplied by ten on the way through. Coordinates are 32-bit words with
//! flag bits in the low byte and a Y axis pointing up, against the XML's Y
//! down. Angles are tenths of a degree counterclockwise, against radians.
//!
//! Writing back to `.ocd` is not supported, and is not planned.

use std::collections::{HashMap, HashSet};

use crate::map::coord_flag;
use crate::ocd_crs;

/// A map converted out of an OCAD file.
#[derive(Debug)]
pub struct Imported {
    /// The map, as the XML of an `.omap` file.
    pub xml: String,
    /// The OCAD format version the file turned out to be: 6 to 12, or 2018.
    pub version: u16,
    /// What the import could carry on past, but thought worth mentioning:
    /// symbols it could not translate, features the XML has no place for.
    pub warnings: Vec<String>,
}

/// Whether the data begins with OCAD's vendor mark.
///
/// This is how an OCAD file is recognized -- not by its name, which anything
/// may be called.
pub fn is_ocd_file(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xad && data[1] == 0x0c
}

/// Converts an OCAD file into the XML of an `.omap` file.
///
/// Returns an error only where nothing can be made of the data at all -- it
/// is not an OCAD file, or it is a version this does not read. Everything
/// recoverable is recovered, and reported in [`Imported::warnings`].
pub fn ocd_to_omap_xml(data: &[u8]) -> Result<Imported, String> {
    if data.len() < 48 || !is_ocd_file(data) {
        return Err("Not an OCD file (missing 0x0cad vendor mark).".to_string());
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if !matches!(version, 6..=12 | 2018) {
        return Err(format!("OCD files of version {version} are not supported."));
    }
    Import::new(data, version).run()
}

// ---------------------------------------------------------------------------
// Layout families

/// Which layout a version's records use.
///
/// The file format changed by growing its records rather than by rearranging
/// them, and it did not change on every release: 6 to 8 share one layout, 9
/// and 10 the next, 11 extends the symbol header again, and 12 and 2018 also
/// move area symbols and objects. Each family is named after its first
/// version.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    V8,
    V9,
    V11,
    V12,
}

impl Family {
    fn of(version: u16) -> Family {
        match version {
            0..=8 => Family::V8,
            9..=10 => Family::V9,
            11 => Family::V11,
            _ => Family::V12,
        }
    }

    /// Offset from a symbol's start to its type-specific payload.
    fn base_symbol_size(self) -> usize {
        match self {
            Family::V8 => 348,
            Family::V9 => 572,
            Family::V11 | Family::V12 => 796,
        }
    }
}

const OCD_TYPE_POINT: u16 = 1;
const OCD_TYPE_LINE: u16 = 2;
const OCD_TYPE_AREA: u16 = 3;
const OCD_TYPE_TEXT: u16 = 4;
const OCD_TYPE_RECTANGLE_V8: u16 = 5;
const OCD_TYPE_LINE_TEXT: u16 = 6;
const OCD_TYPE_RECTANGLE_V9: u16 = 7;

/// The distance from a circle's quadrant to its bezier control point, as a
/// fraction of the radius. Four such curves make a round corner.
const BEZIER_KAPPA: f64 = 0.552_284_749_830_793_6;

// ---------------------------------------------------------------------------
// Text

/// The 32 characters where windows-1252 parts company with Latin-1, at 0x80
/// to 0x9f. The five holes in that range stand for themselves, which is what
/// the WHATWG encoding standard asks for and what a browser does.
const CP1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

/// Decodes the single-byte encoding OCAD wrote before it moved to Unicode.
///
/// The format calls it "the system code page", which cannot be recovered from
/// the file; windows-1252 is what Mapper assumes, is right for the files this
/// was tested against, and leaves every ASCII byte alone in any case.
fn decode_cp1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9f => CP1252_HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

/// Decodes UTF-16, little end first, replacing anything malformed.
fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Everything up to the first zero byte.
fn until_nul(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&b| b == 0) {
        Some(i) => &bytes[..i],
        None => bytes,
    }
}

/// Line endings as the XML wants them.
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Escapes text for XML, and drops the control characters XML has no way to
/// carry -- OCAD lets them into a name, and a file with one in it must still
/// come out readable.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' => {}
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Numbers

/// Rounds a half up, towards positive infinity.
///
/// Not `f64::round`, which rounds a half away from zero: the reference
/// importers round the other way, and a coordinate on a half unit would land
/// one unit out on the negative side of the map.
fn round_half_up(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Writes a number for an XML attribute, at the seven decimals the format is
/// worth reading to.
///
/// Rust's shortest round-trip formatting means the text parses back as the
/// number it was written from. It never uses exponential notation, so a value
/// too small to write plainly -- below a millionth, which no length, angle or
/// coordinate here reaches -- comes out as a long decimal rather than in the
/// short form other writers would choose.
fn num(x: f64) -> String {
    let rounded = round_half_up(x * 1e7) / 1e7;
    // Nothing is served by writing a negative zero into a file.
    if rounded == 0.0 {
        return "0".to_string();
    }
    format!("{rounded}")
}

/// Reads the leading number out of a string, as a parameter string's fields
/// are written: a number, sometimes with something after it.
fn leading_f64(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut end = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut seen_exp = false;
    while end < bytes.len() {
        match bytes[end] {
            b'0'..=b'9' => seen_digit = true,
            b'+' | b'-' => {
                let start_of_number = end == 0;
                let after_exponent = end > 0 && matches!(bytes[end - 1], b'e' | b'E');
                if !start_of_number && !after_exponent {
                    break;
                }
            }
            b'.' if !seen_dot && !seen_exp => seen_dot = true,
            b'e' | b'E' if seen_digit && !seen_exp => seen_exp = true,
            _ => break,
        }
        end += 1;
    }
    if !seen_digit {
        return None;
    }
    // Back off a trailing exponent marker or sign with no digits behind it.
    while end > 0 && !bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    s[..end].parse().ok()
}

// ---------------------------------------------------------------------------
// The file

/// One symbol, translated far enough that only its wrapper is still to write.
struct ParsedSymbol {
    /// The symbol number OCAD knows it by, and objects refer to it with.
    number: i64,
    /// The symbol's id in the XML being written: its position in the file.
    id: usize,
    /// The number as a symbol code, e.g. `114.1`.
    code: String,
    name: String,
    /// Switched off in OCAD's symbol set. The XML has no such state, so the
    /// symbol is imported as a visible one and the fact is reported.
    hidden: bool,
    /// Which kind of object the XML gives this symbol: 0 point, 1 path, 4 text.
    object_type: u8,
    /// The symbol's body, without the wrapper the border may still need.
    body_xml: String,
    /// The XML `type` of `body_xml`: 1 point, 2 line, 4 area, 8 text, 16 combined.
    omap_type: u8,
    /// An area symbol's border line, by OCAD symbol number, where it has one.
    border_symbol_number: Option<i64>,
    /// Text symbols: the alignment an object drawn with it takes.
    h_align: u8,
    v_align: u8,
    /// Rectangle symbols: the corner radius its objects are built with.
    rect_corner_radius: Option<i64>,
}

struct Import<'a> {
    bytes: &'a [u8],
    version: u16,
    family: Family,
    /// What a symbol number is divided by to separate its main number from its
    /// sub-number: a hundredth of the number in the oldest files, a thousandth
    /// since.
    number_factor: i64,
    warnings: Vec<String>,
    warned: HashSet<String>,
    /// OCAD colour number to the priority it was given in the XML.
    color_index: HashMap<i64, usize>,
    /// Symbols in file order, and where to find one by its OCAD number.
    symbols: Vec<ParsedSymbol>,
    symbol_by_number: HashMap<i64, usize>,
}

/// What a read past the end of the file gives back. The file is truncated or
/// its offsets are wrong, and the record being read is abandoned.
type Rd<T> = Result<T, String>;

impl<'a> Import<'a> {
    fn new(bytes: &'a [u8], version: u16) -> Import<'a> {
        let family = Family::of(version);
        Import {
            bytes,
            version,
            family,
            number_factor: if family == Family::V8 { 10 } else { 1000 },
            warnings: Vec::new(),
            warned: HashSet::new(),
            color_index: HashMap::new(),
            symbols: Vec::new(),
            symbol_by_number: HashMap::new(),
        }
    }

    fn run(mut self) -> Result<Imported, String> {
        let colors_xml = self.import_colors();
        self.import_symbols();
        let objects_xml = self.import_objects();
        let georef_xml = self.import_georeferencing();
        let notes = self.import_notes();

        // A border naming a symbol which is not in the file: the area is
        // still imported, without it.
        let orphan_borders: Vec<(String, String, i64)> = self
            .symbols
            .iter()
            .filter_map(|sym| {
                let border = sym.border_symbol_number?;
                (!self.symbol_by_number.contains_key(&border))
                    .then(|| (sym.code.clone(), sym.name.clone(), border))
            })
            .collect();
        for (code, name, border) in orphan_borders {
            self.warn(format!(
                "In area symbol {code} '{name}': border line symbol {border} not found."
            ));
        }

        let symbols_xml: Vec<String> = (0..self.symbols.len())
            .map(|i| self.finish_symbol_xml(i))
            .collect();

        let hidden: Vec<&str> = self
            .symbols
            .iter()
            .filter(|s| s.hidden)
            .map(|s| s.code.as_str())
            .collect();
        if !hidden.is_empty() {
            let message = format!(
                "{} symbol(s) marked hidden in OCAD are imported as visible: {}",
                hidden.len(),
                hidden.join(", ")
            );
            self.warn(message);
        }

        let mut xml = String::with_capacity(objects_xml.len() * 64 + symbols_xml.len() * 512);
        xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        xml.push_str(
            "<map xmlns=\"http://openorienteering.org/apps/mapper/xml/v2\" version=\"9\">\n",
        );
        xml.push_str(&format!("<notes>{}</notes>\n", xml_escape(&notes)));
        xml.push_str(&georef_xml);
        xml.push_str(&format!(
            "<colors count=\"{}\">\n{}</colors>\n",
            self.color_index.len(),
            colors_xml
        ));
        xml.push_str("<barrier version=\"6\" required=\"0.6.0\">\n");
        xml.push_str(&format!(
            "<symbols count=\"{}\" id=\"OCD\">\n{}\n</symbols>\n",
            symbols_xml.len(),
            symbols_xml.join("\n")
        ));
        xml.push_str("<parts count=\"1\" current=\"0\">\n");
        xml.push_str(&format!(
            "<part name=\"default part\"><objects count=\"{}\">\n{}\n</objects></part>\n",
            objects_xml.len(),
            objects_xml.join("\n")
        ));
        xml.push_str("</parts>\n</barrier>\n</map>\n");

        Ok(Imported {
            xml,
            version: self.version,
            warnings: self.warnings,
        })
    }

    /// Records something worth telling the user, once however often it happens.
    fn warn(&mut self, message: String) {
        if self.warned.insert(message.clone()) {
            self.warnings.push(message);
        }
    }

    // -- low-level readers --------------------------------------------------

    fn slice(&self, off: usize, len: usize) -> Rd<&'a [u8]> {
        self.bytes
            .get(off..off.saturating_add(len))
            .ok_or_else(|| format!("read past the end of the file at {off}"))
    }

    fn u8(&self, off: usize) -> Rd<u8> {
        Ok(self.slice(off, 1)?[0])
    }
    fn u16(&self, off: usize) -> Rd<u16> {
        let b = self.slice(off, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&self, off: usize) -> Rd<i16> {
        Ok(self.u16(off)? as i16)
    }
    fn u32(&self, off: usize) -> Rd<u32> {
        let b = self.slice(off, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&self, off: usize) -> Rd<i32> {
        Ok(self.u32(off)? as i32)
    }
    fn f64(&self, off: usize) -> Rd<f64> {
        let b = self.slice(off, 8)?;
        Ok(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// An offset which is present (zero means absent) and whose record fits.
    fn in_bounds(&self, off: usize, size: usize) -> bool {
        off > 0 && off.saturating_add(size) <= self.bytes.len()
    }

    /// A length byte, then that many bytes of the single-byte encoding.
    fn pascal_cp1252(&self, off: usize, max: usize) -> Rd<String> {
        let len = (self.u8(off)? as usize).min(max);
        Ok(decode_cp1252(self.slice(off + 1, len)?))
    }

    /// A length byte, then that many bytes of UTF-8 (version 11 and later).
    fn pascal_utf8(&self, off: usize, max: usize) -> Rd<String> {
        let len = (self.u8(off)? as usize).min(max);
        Ok(String::from_utf8_lossy(self.slice(off + 1, len)?).into_owned())
    }

    /// UTF-16, zero terminated, of at most `max` code units.
    fn utf16(&self, off: usize, max: usize) -> Rd<String> {
        let mut end = off;
        let limit = off + 2 * max;
        while end < limit && self.u16(end)? != 0 {
            end += 2;
        }
        Ok(decode_utf16le(self.slice(off, end - off)?))
    }

    /// One coordinate pair: the flag bits out of the low bytes, and the
    /// position in the XML's units and with its Y axis.
    fn coord(&self, off: usize) -> Rd<OcdCoord> {
        let rx = self.i32(off)?;
        let ry = self.i32(off + 4)?;
        let mut ox = rx >> 8;
        let mut oy = ry >> 8;
        // Mapper 0.6.2 to 0.6.4 wrote this in place of a tiny negative value;
        // see its own convertOcdPoint.
        let invalid = i32::MIN >> 8;
        if ox == invalid {
            ox = 0;
        }
        if oy == invalid {
            oy = 0;
        }
        Ok(OcdCoord {
            x: ox as i64 * 10,
            y: oy as i64 * -10,
            xf: (rx & 0xff) as u8,
            yf: (ry & 0xff) as u8,
        })
    }

    /// An OCAD colour number as the XML's reference to it: the priority it was
    /// given, or `-1` for a colour the file never defined.
    fn color_ref(&mut self, ocd_color: i64) -> String {
        match self.color_index.get(&ocd_color) {
            Some(&priority) => priority.to_string(),
            None => {
                self.warn(format!(
                    "Color id not found: {ocd_color}, ignoring this color."
                ));
                "-1".to_string()
            }
        }
    }
}

/// A coordinate as the file gives it: the position in the XML's units, and the
/// flag bits still in the form OCAD wrote them.
#[derive(Clone, Copy, Default)]
struct OcdCoord {
    x: i64,
    y: i64,
    xf: u8,
    yf: u8,
}

// ---------------------------------------------------------------------------
// Parameter strings

/// A parameter string, split as OCAD writes them: a first field of its own,
/// then any number of fields each introduced by a one-character key.
struct ParamString {
    first: String,
    params: Vec<(char, String)>,
}

impl ParamString {
    fn parse(s: &str) -> ParamString {
        let mut fields = s.split('\t');
        let first = fields.next().unwrap_or_default().to_string();
        let params = fields
            .filter_map(|field| {
                let mut chars = field.chars();
                let key = chars.next()?;
                Some((key, chars.as_str().to_string()))
            })
            .collect();
        ParamString { first, params }
    }

    /// The leading number of the field with this key, if it has one.
    fn number(&self, key: char) -> Option<f64> {
        self.params
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| leading_f64(v))
    }
}

impl Import<'_> {
    /// Every parameter string of one type, in file order.
    ///
    /// They are OCAD's extension mechanism: a chain of blocks of 256 entries,
    /// each pointing at a piece of text whose meaning is its type number. The
    /// colours of a modern file are in here, and so is its georeferencing.
    fn param_strings(&self, string_type: i32) -> Vec<String> {
        let mut out = Vec::new();
        if self.version < 8 {
            return out;
        }
        // first_string_block, at the same offset in both header layouts.
        let mut block_pos = self.u32(32).unwrap_or(0) as usize;
        let mut seen = HashSet::new();
        while block_pos != 0 && self.in_bounds(block_pos, 4 + 256 * 16) {
            // A file whose block chain loops would otherwise never be done.
            if !seen.insert(block_pos) {
                break;
            }
            for i in 0..256 {
                let e = block_pos + 4 + i * 16;
                let (Ok(pos), Ok(size), Ok(entry_type)) =
                    (self.u32(e), self.u32(e + 4), self.i32(e + 8))
                else {
                    continue;
                };
                if entry_type != string_type || pos == 0 || size == 0 {
                    continue;
                }
                let (pos, size) = (pos as usize, size as usize);
                if !self.in_bounds(pos, size) {
                    continue;
                }
                let Ok(raw) = self.slice(pos, size) else {
                    continue;
                };
                let raw = until_nul(raw);
                out.push(if self.family >= Family::V11 {
                    String::from_utf8_lossy(raw).into_owned()
                } else {
                    decode_cp1252(raw)
                });
            }
            block_pos = self.u32(block_pos).unwrap_or(0) as usize;
        }
        out
    }

    // -- colours ------------------------------------------------------------

    /// The colour table, in the order it is drawn in.
    ///
    /// A colour's position in that order is its priority, which is what the
    /// XML refers to a colour by, so the order is the thing that must be kept.
    fn import_colors(&mut self) -> String {
        let mut out = String::new();
        if self.family == Family::V8 {
            // Up to version 8 the colours are in the header's symbol block.
            let count = (self.u16(48).unwrap_or(0) as usize).min(256);
            for i in 0..count {
                let off = 72 + i * 72;
                if !self.in_bounds(off, 72) {
                    break;
                }
                let (Ok(number), Ok(c), Ok(m), Ok(y), Ok(k), Ok(name)) = (
                    self.u16(off),
                    self.u8(off + 4),
                    self.u8(off + 5),
                    self.u8(off + 6),
                    self.u8(off + 7),
                    self.pascal_cp1252(off + 8, 31),
                ) else {
                    break;
                };
                // Ink is stored as a whole number of half percents.
                let ink = |v: u8| f64::from(v) / 200.0;
                self.emit_color(
                    &mut out,
                    &name,
                    ink(c),
                    ink(m),
                    ink(y),
                    ink(k),
                    1.0,
                    i64::from(number),
                );
            }
        } else {
            // From version 9 a colour is a parameter string, and the strings
            // are already in drawing order.
            for s in self.param_strings(9) {
                let parsed = ParamString::parse(&s);
                let Some(number) = parsed.number('n') else {
                    continue;
                };
                // Percentages, and only a real one counts.
                let percent = |key: char, default: f64| match parsed.number(key) {
                    Some(v) if (0.0..=100.0).contains(&v) => v / 100.0,
                    _ => default,
                };
                let (c, m, y, k, opacity) = (
                    percent('c', 0.0),
                    percent('m', 0.0),
                    percent('y', 0.0),
                    percent('k', 0.0),
                    percent('t', 1.0),
                );
                let name = parsed.first.clone();
                self.emit_color(&mut out, &name, c, m, y, k, opacity, number as i64);
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_color(
        &mut self,
        out: &mut String,
        name: &str,
        c: f64,
        m: f64,
        y: f64,
        k: f64,
        opacity: f64,
        number: i64,
    ) {
        let priority = self.color_index.len();
        self.color_index.insert(number, priority);
        out.push_str(&format!(
            "<color priority=\"{}\" name=\"{}\" c=\"{}\" m=\"{}\" y=\"{}\" k=\"{}\" opacity=\"{}\"/>\n",
            priority,
            xml_escape(name),
            num(c),
            num(m),
            num(y),
            num(k),
            num(opacity)
        ));
    }
}

impl PartialOrd for Family {
    fn partial_cmp(&self, other: &Family) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Family {
    fn cmp(&self, other: &Family) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl Family {
    fn rank(self) -> u8 {
        match self {
            Family::V8 => 8,
            Family::V9 => 9,
            Family::V11 => 11,
            Family::V12 => 12,
        }
    }
}

// ---------------------------------------------------------------------------
// Symbols

impl Import<'_> {
    /// Walks the chain of symbol blocks and translates every symbol in them.
    fn import_symbols(&mut self) {
        let mut block_pos = self.u32(8).unwrap_or(0) as usize;
        let mut next_id = 0usize;
        let mut seen = HashSet::new();
        while block_pos != 0 && self.in_bounds(block_pos, 4 + 256 * 4) {
            if !seen.insert(block_pos) {
                break;
            }
            for i in 0..256 {
                let pos = self.u32(block_pos + 4 + i * 4).unwrap_or(0) as usize;
                if pos == 0 || !self.in_bounds(pos, self.family.base_symbol_size()) {
                    continue;
                }
                match self.parse_symbol(pos, next_id) {
                    Ok(Some(sym)) => {
                        let number = sym.number;
                        match self.symbol_by_number.get(&number) {
                            // A number used twice keeps the place of the first.
                            Some(&at) => self.symbols[at] = sym,
                            None => {
                                self.symbol_by_number.insert(number, self.symbols.len());
                                self.symbols.push(sym);
                            }
                        }
                        next_id += 1;
                    }
                    Ok(None) => {}
                    Err(err) => self.warn(format!(
                        "Skipping unreadable symbol at file position {pos}: {err}"
                    )),
                }
            }
            block_pos = self.u32(block_pos).unwrap_or(0) as usize;
        }
    }

    /// One symbol: its header, then the payload its type calls for.
    fn parse_symbol(&mut self, pos: usize, id: usize) -> Rd<Option<ParsedSymbol>> {
        let f = self.family;
        let (number, ocd_type, flags, status, name) = if f == Family::V8 {
            let number = i64::from(self.u16(pos + 2)?);
            let mut ocd_type = self.u16(pos + 4)?;
            // Before version 9 a line text symbol is a line symbol with a
            // second type byte set.
            if ocd_type == OCD_TYPE_LINE && self.u8(pos + 6)? == 1 {
                ocd_type = OCD_TYPE_LINE_TEXT;
            }
            (
                number,
                ocd_type,
                self.u8(pos + 7)?,
                self.u8(pos + 11)?,
                self.pascal_cp1252(pos + 52, 31)?,
            )
        } else {
            (
                i64::from(self.u32(pos + 4)?),
                u16::from(self.u8(pos + 8)?),
                self.u8(pos + 9)?,
                self.u8(pos + 11)?,
                if f == Family::V9 {
                    self.pascal_cp1252(pos + 56, 31)?
                } else {
                    self.utf16(pos + 56, 64)?
                },
            )
        };

        let main = number / self.number_factor;
        let sub = number % self.number_factor;
        let code = if sub == 0 {
            main.to_string()
        } else {
            format!("{main}.{sub}")
        };
        let rotatable = flags & 0x01 != 0;

        let mut sym = ParsedSymbol {
            number,
            id,
            code: code.clone(),
            name: name.clone(),
            hidden: status & 0x02 != 0,
            object_type: 1,
            body_xml: String::new(),
            omap_type: 2,
            border_symbol_number: None,
            h_align: 0,
            v_align: 0,
            rect_corner_radius: None,
        };

        let payload = pos + f.base_symbol_size();
        match ocd_type {
            OCD_TYPE_POINT => {
                let data_size = self.u16(payload)? as usize;
                sym.body_xml = self.point_symbol_xml(data_size, payload + 4, rotatable)?;
                sym.omap_type = 1;
                sym.object_type = 0;
            }
            OCD_TYPE_LINE => {
                let common = self.read_line_common(payload)?;
                let (xml, omap_type) = self.line_symbol_xml(&common, payload + 76, &code)?;
                sym.body_xml = xml;
                sym.omap_type = omap_type;
            }
            OCD_TYPE_AREA => {
                let fill_on;
                let common_off;
                let data_size_off;
                let mut border_symbol = 0i64;
                let mut border_on = false;
                if f == Family::V8 {
                    fill_on = self.u16(payload + 2)? != 0;
                    common_off = payload + 4;
                    data_size_off = payload + 30;
                } else {
                    border_symbol = i64::from(self.u32(payload)?);
                    common_off = payload + 4;
                    fill_on = self.u8(common_off + 14)? != 0;
                    border_on = self.u8(common_off + 15)? != 0;
                    data_size_off = if f == Family::V12 {
                        common_off + 30
                    } else {
                        common_off + 26
                    };
                }
                let data_size = self.u16(data_size_off)? as usize;
                sym.body_xml =
                    self.area_symbol_xml(fill_on, common_off, data_size, data_size_off + 2)?;
                sym.omap_type = 4;
                if border_on && f != Family::V8 {
                    if border_symbol == number {
                        self.warn(format!(
                            "In area symbol {code} '{name}': the border of this symbol could not be loaded."
                        ));
                    } else {
                        sym.border_symbol_number = Some(border_symbol);
                    }
                }
            }
            OCD_TYPE_TEXT | OCD_TYPE_LINE_TEXT => {
                let font_off = payload;
                let font_name = if f >= Family::V11 {
                    self.pascal_utf8(font_off, 31)?
                } else {
                    self.pascal_cp1252(font_off, 31)?
                };
                let text = self.text_symbol_xml(
                    &font_name,
                    font_off + 32,
                    ocd_type == OCD_TYPE_TEXT,
                    &code,
                    &name,
                )?;
                sym.body_xml = text.xml;
                sym.omap_type = 8;
                sym.object_type = 4;
                sym.h_align = text.h_align;
                sym.v_align = text.v_align;
                if ocd_type == OCD_TYPE_LINE_TEXT {
                    self.warn(format!(
                        "Line text symbol {code} '{name}' is imported as a plain text symbol (text-on-path is not supported)."
                    ));
                }
            }
            OCD_TYPE_RECTANGLE_V8 | OCD_TYPE_RECTANGLE_V9 => {
                let line_color = i64::from(self.u16(payload)?);
                let line_width = i64::from(self.u16(payload + 2)?);
                let corner_radius = i64::from(self.u16(payload + 4)?);
                let grid_flags = self.u16(payload + 6)?;
                let color = if line_width != 0 {
                    self.color_ref(line_color)
                } else {
                    "-1".to_string()
                };
                sym.body_xml = format!(
                    "<line_symbol color=\"{color}\" line_width=\"{}\" cap_style=\"1\" join_style=\"2\"/>",
                    line_width * 10
                );
                sym.omap_type = 2;
                sym.rect_corner_radius = Some(corner_radius * 10);
                if grid_flags & 1 != 0 {
                    self.warn(format!(
                        "Rectangle symbol {code} '{name}': cell grid and numbering are not imported."
                    ));
                }
            }
            other => {
                self.warn(format!(
                    "Unable to import symbol {code} '{name}': unsupported type {other}."
                ));
                return Ok(None);
            }
        }
        Ok(Some(sym))
    }

    /// Wraps a translated symbol in its `<symbol>` element.
    ///
    /// An area with a border is the one symbol which cannot be written on its
    /// own: OCAD gives the area a line symbol to draw round it, and the XML
    /// has no such field, so the two become a combined symbol -- the area
    /// private to it, the border a reference to the shared line symbol.
    fn finish_symbol_xml(&self, index: usize) -> String {
        let sym = &self.symbols[index];
        let attrs = |ty: u8| {
            format!(
                "type=\"{ty}\" id=\"{}\" code=\"{}\" name=\"{}\"",
                sym.id,
                xml_escape(&sym.code),
                xml_escape(&sym.name)
            )
        };
        if let Some(border_number) = sym.border_symbol_number {
            if let Some(&at) = self.symbol_by_number.get(&border_number) {
                return format!(
                    "<symbol {}><combined_symbol parts=\"2\">\
                     <part private=\"true\"><symbol type=\"{}\" code=\"\">{}</symbol></part>\
                     <part symbol=\"{}\"/>\
                     </combined_symbol></symbol>",
                    attrs(16),
                    sym.omap_type,
                    sym.body_xml,
                    self.symbols[at].id
                );
            }
        }
        format!("<symbol {}>{}</symbol>", attrs(sym.omap_type), sym.body_xml)
    }
}

// ---------------------------------------------------------------------------
// Point symbols, and the little glyphs a line or an area is decorated with

/// The dot and the ring an XML point symbol carries directly, before any
/// elements: what the first origin-centred piece of an OCAD point symbol
/// folds into.
#[derive(Clone)]
struct PointBase {
    inner_radius: i64,
    inner_color: String,
    outer_width: i64,
    outer_color: String,
}

impl Default for PointBase {
    fn default() -> PointBase {
        PointBase {
            inner_radius: 0,
            inner_color: "-1".to_string(),
            outer_width: 0,
            outer_color: "-1".to_string(),
        }
    }
}

impl Import<'_> {
    /// Translates the element records of a point symbol.
    ///
    /// After Mapper's `setupPointSymbolPattern`: the first dot or circle
    /// sitting at the origin becomes the symbol's own dot or ring, and
    /// everything else -- further dots, lines, filled areas -- becomes an
    /// element drawn with a symbol of its own.
    ///
    /// The same records describe the glyphs on a line symbol and the pattern
    /// of an area symbol, which is why this is not only used for point
    /// symbols.
    fn point_symbol_xml(
        &mut self,
        data_size: usize,
        elements_off: usize,
        rotatable: bool,
    ) -> Rd<String> {
        let mut base = PointBase::default();
        let mut base_used = false;
        let mut elements: Vec<String> = Vec::new();

        let mut i = 0usize;
        while i < data_size {
            let off = elements_off + i * 8;
            if !self.in_bounds(off, 16) {
                break;
            }
            let element_type = self.u16(off)?;
            let flags = self.u16(off + 2)?;
            let color = i64::from(self.u16(off + 4)?);
            let line_width = i64::from(self.i16(off + 6)?);
            let diameter = i64::from(self.i16(off + 8)?);
            let num_coords = self.u16(off + 10)? as usize;
            let coords_off = off + 16;
            if num_coords > 0 && !self.in_bounds(coords_off, num_coords * 8) {
                break;
            }

            // A piece the symbol is built around, rather than one placed
            // somewhere in it.
            let at_origin =
                num_coords == 0 || (self.i32(coords_off)? == 0 && self.i32(coords_off + 4)? == 0);
            let position = |this: &Self| -> Rd<OcdCoord> {
                if num_coords > 0 {
                    this.coord(coords_off)
                } else {
                    Ok(OcdCoord::default())
                }
            };

            match element_type {
                4 => {
                    // A filled dot.
                    if diameter > 0 {
                        let dot = PointBase {
                            inner_radius: diameter * 10 / 2,
                            inner_color: self.color_ref(color),
                            outer_width: 0,
                            outer_color: "-1".to_string(),
                        };
                        if !base_used && at_origin {
                            base = dot;
                            base_used = true;
                        } else {
                            let at = position(self)?;
                            elements.push(point_element_xml(&dot, at.x, at.y));
                        }
                    }
                }
                3 => {
                    // A ring. Which of the two ways to read the diameter is
                    // right changed with version 9.
                    let radius = if self.family == Family::V8 {
                        diameter / 2 - line_width
                    } else {
                        (diameter - line_width) / 2
                    };
                    if radius > 0 && line_width > 0 {
                        let ring = PointBase {
                            inner_radius: radius * 10,
                            inner_color: "-1".to_string(),
                            outer_width: line_width * 10,
                            outer_color: self.color_ref(color),
                        };
                        if !base_used && at_origin {
                            base = ring;
                            base_used = true;
                        } else {
                            let at = position(self)?;
                            elements.push(point_element_xml(&ring, at.x, at.y));
                        }
                    }
                }
                1 => {
                    // A stroke. The element's flags set the cap and the join
                    // together rather than one each.
                    if line_width > 0 {
                        let (cap, join) = match flags {
                            1 => (1, 2),
                            4 => (0, 1),
                            _ => (0, 0),
                        };
                        let color = self.color_ref(color);
                        let coords = self.path_coord_string(coords_off, num_coords, false)?;
                        elements.push(format!(
                            "<element><symbol type=\"2\" code=\"\"><line_symbol color=\"{color}\" line_width=\"{}\" cap_style=\"{cap}\" join_style=\"{join}\"/></symbol>\
                             <object type=\"1\"><coords count=\"{num_coords}\">{coords}</coords></object></element>",
                            line_width * 10
                        ));
                    }
                }
                2 => {
                    // A filled shape.
                    let color = self.color_ref(color);
                    let coords = self.path_coord_string(coords_off, num_coords, true)?;
                    elements.push(format!(
                        "<element><symbol type=\"4\" code=\"\"><area_symbol inner_color=\"{color}\" min_area=\"0\" patterns=\"0\"/></symbol>\
                         <object type=\"1\"><coords count=\"{num_coords}\">{coords}</coords></object></element>"
                    ));
                }
                other => self.warn(format!("Unsupported point symbol element type {other}.")),
            }
            i += 2 + num_coords;
        }

        Ok(format!(
            "<point_symbol rotatable=\"{rotatable}\" inner_radius=\"{}\" inner_color=\"{}\" outer_width=\"{}\" outer_color=\"{}\" elements=\"{}\">{}</point_symbol>",
            base.inner_radius,
            base.inner_color,
            base.outer_width,
            base.outer_color,
            elements.len(),
            elements.concat()
        ))
    }
}

/// A dot or a ring placed somewhere other than the middle of its symbol.
fn point_element_xml(p: &PointBase, x: i64, y: i64) -> String {
    format!(
        "<element><symbol type=\"1\" code=\"\"><point_symbol rotatable=\"false\" inner_radius=\"{}\" inner_color=\"{}\" outer_width=\"{}\" outer_color=\"{}\"/></symbol>\
         <object type=\"0\"><coords count=\"1\">{x} {y};</coords></object></element>",
        p.inner_radius, p.inner_color, p.outer_width, p.outer_color
    )
}

// ---------------------------------------------------------------------------
// Line symbols

/// The line record, as the file lays it out.
///
/// One OCAD line symbol describes as many as three strokes at once -- the
/// line itself, a "double" line drawn as a pair of borders, and a framing
/// line under both -- along with the glyphs placed along it.
struct LineCommon {
    line_color: i64,
    line_width: i64,
    line_style: u16,
    dist_from_start: i64,
    dist_from_end: i64,
    main_length: i64,
    end_length: i64,
    main_gap: i64,
    sec_gap: i64,
    num_prim_sym: i64,
    prim_sym_dist: i64,
    double_mode: u16,
    double_flags: u16,
    double_color: i64,
    double_left_color: i64,
    double_right_color: i64,
    double_width: i64,
    double_left_width: i64,
    double_right_width: i64,
    double_length: i64,
    double_gap: i64,
    framing_color: i64,
    framing_width: i64,
    framing_style: u16,
    primary_data_size: usize,
    secondary_data_size: usize,
    corner_data_size: usize,
    start_data_size: usize,
    end_data_size: usize,
}

/// One border of a double line.
struct BorderFields {
    color: String,
    width: i64,
    shift: i64,
    dashed: bool,
    dash_length: i64,
    break_length: i64,
}

/// A line symbol as the XML describes one, which is what an OCAD line is
/// translated into -- possibly more than one of them.
struct LineFields {
    color: String,
    line_width: i64,
    cap_style: u8,
    join_style: u8,
    start_offset: i64,
    end_offset: i64,
    dashed: bool,
    dash_length: i64,
    break_length: i64,
    dashes_in_group: i64,
    in_group_break_length: i64,
    half_outer_dashes: bool,
    segment_length: i64,
    end_length: i64,
    show_at_least_one_symbol: bool,
    mid_symbols_per_spot: i64,
    mid_symbol_distance: i64,
    mid_symbol_placement: u8,
    /// Inline `<symbol>` XML of a glyph, or empty where there is none.
    mid_symbol: String,
    dash_symbol: String,
    start_symbol: String,
    end_symbol: String,
    borders: Vec<BorderFields>,
}

impl LineFields {
    /// A line which draws nothing, and the defaults the XML expects of the
    /// fields that are not set from the file.
    fn empty() -> LineFields {
        LineFields {
            color: "-1".to_string(),
            line_width: 0,
            cap_style: 0,
            join_style: 0,
            start_offset: 0,
            end_offset: 0,
            dashed: false,
            dash_length: 4000,
            break_length: 1000,
            dashes_in_group: 1,
            in_group_break_length: 500,
            half_outer_dashes: false,
            segment_length: 4000,
            end_length: 0,
            show_at_least_one_symbol: false,
            mid_symbols_per_spot: 1,
            mid_symbol_distance: 0,
            mid_symbol_placement: 0,
            mid_symbol: String::new(),
            dash_symbol: String::new(),
            start_symbol: String::new(),
            end_symbol: String::new(),
            borders: Vec::new(),
        }
    }

    /// Whether this line puts any ink on the map.
    fn visible(&self) -> bool {
        (self.line_width > 0 && self.color != "-1") || !self.mid_symbol.is_empty()
    }

    fn to_xml(&self) -> String {
        let mut xml = format!(
            "<line_symbol color=\"{}\" line_width=\"{}\" join_style=\"{}\" cap_style=\"{}\" start_offset=\"{}\" end_offset=\"{}\" segment_length=\"{}\" end_length=\"{}\" show_at_least_one_symbol=\"{}\" dashed=\"{}\" dash_length=\"{}\" break_length=\"{}\" dashes_in_group=\"{}\" in_group_break_length=\"{}\" half_outer_dashes=\"{}\" mid_symbols_per_spot=\"{}\" mid_symbol_distance=\"{}\" mid_symbol_placement=\"{}\"",
            self.color,
            self.line_width,
            self.join_style,
            self.cap_style,
            self.start_offset,
            self.end_offset,
            self.segment_length,
            self.end_length,
            self.show_at_least_one_symbol,
            self.dashed,
            self.dash_length,
            self.break_length,
            self.dashes_in_group,
            self.in_group_break_length,
            self.half_outer_dashes,
            self.mid_symbols_per_spot,
            self.mid_symbol_distance,
            self.mid_symbol_placement
        );

        let mut inner = String::new();
        let wrap = |tag: &str, body: &str| {
            if body.is_empty() {
                String::new()
            } else {
                format!("<{tag}>{body}</{tag}>")
            }
        };
        inner.push_str(&wrap("start_symbol", &self.start_symbol));
        inner.push_str(&wrap("mid_symbol", &self.mid_symbol));
        inner.push_str(&wrap("end_symbol", &self.end_symbol));
        inner.push_str(&wrap("dash_symbol", &self.dash_symbol));
        if !self.borders.is_empty() {
            inner.push_str("<borders>");
            for b in &self.borders {
                inner.push_str(&format!(
                    "<border color=\"{}\" width=\"{}\" shift=\"{}\"",
                    b.color, b.width, b.shift
                ));
                if b.dashed {
                    inner.push_str(&format!(
                        " dashed=\"true\" dash_length=\"{}\" break_length=\"{}\"",
                        b.dash_length, b.break_length
                    ));
                }
                inner.push_str("/>");
            }
            inner.push_str("</borders>");
        }

        if inner.is_empty() {
            xml.push_str("/>");
        } else {
            xml.push('>');
            xml.push_str(&inner);
            xml.push_str("</line_symbol>");
        }
        xml
    }
}

impl Import<'_> {
    fn read_line_common(&self, off: usize) -> Rd<LineCommon> {
        Ok(LineCommon {
            line_color: i64::from(self.u16(off)?),
            line_width: i64::from(self.u16(off + 2)?),
            line_style: self.u16(off + 4)?,
            dist_from_start: i64::from(self.i16(off + 6)?),
            dist_from_end: i64::from(self.i16(off + 8)?),
            main_length: i64::from(self.i16(off + 10)?),
            end_length: i64::from(self.i16(off + 12)?),
            main_gap: i64::from(self.i16(off + 14)?),
            sec_gap: i64::from(self.i16(off + 16)?),
            // +18 is the end gap, which the XML has no field for.
            num_prim_sym: i64::from(self.i16(off + 22)?),
            prim_sym_dist: i64::from(self.i16(off + 24)?),
            double_mode: self.u16(off + 26)?,
            double_flags: self.u16(off + 28)?,
            double_color: i64::from(self.u16(off + 30)?),
            double_left_color: i64::from(self.u16(off + 32)?),
            double_right_color: i64::from(self.u16(off + 34)?),
            double_width: i64::from(self.i16(off + 36)?),
            double_left_width: i64::from(self.i16(off + 38)?),
            double_right_width: i64::from(self.i16(off + 40)?),
            double_length: i64::from(self.i16(off + 42)?),
            double_gap: i64::from(self.i16(off + 44)?),
            framing_color: i64::from(self.u16(off + 58)?),
            framing_width: i64::from(self.i16(off + 60)?),
            framing_style: self.u16(off + 62)?,
            primary_data_size: self.u16(off + 64)? as usize,
            secondary_data_size: self.u16(off + 66)? as usize,
            corner_data_size: self.u16(off + 68)? as usize,
            start_data_size: self.u16(off + 70)? as usize,
            end_data_size: self.u16(off + 72)? as usize,
        })
    }

    /// Translates a line symbol, which may come out as more than one.
    ///
    /// After Mapper's `importLineSymbol`. Where the OCAD line draws only one
    /// of its three strokes, that stroke is the symbol; where it draws
    /// several, they become the private parts of a combined symbol, since a
    /// line symbol in the XML is one stroke and no more.
    fn line_symbol_xml(
        &mut self,
        c: &LineCommon,
        elements_off: usize,
        code: &str,
    ) -> Rd<(String, u8)> {
        let mut main = self.line_base(c, code);
        self.line_sub_symbols(&mut main, c, elements_off)?;

        // The framing line, drawn under everything else. Old enough files
        // have no such field, and a zero where it would be.
        let mut framing: Option<LineFields> = None;
        if c.framing_width > 0 && self.version >= 7 {
            let into_main = !main.visible();
            let mut spare = LineFields::empty();
            let color = if c.framing_width > 0 {
                self.color_ref(c.framing_color)
            } else {
                "-1".to_string()
            };
            let target = if into_main { &mut main } else { &mut spare };
            target.line_width = c.framing_width * 10;
            target.color = color;
            match c.framing_style {
                0 => {
                    target.join_style = 0;
                    target.cap_style = 0;
                }
                1 => {
                    target.join_style = 2;
                    target.cap_style = 1;
                }
                4 => {
                    target.join_style = 1;
                    target.cap_style = 0;
                }
                other => self.warn(format!(
                    "In line symbol {code}: unsupported framing line style '{other}'."
                )),
            }
            if !into_main {
                framing = Some(spare);
            }
        }

        // The double line: a pair of borders, with an optional fill between
        // them. Its own dashes are separate from the main line's.
        let mut double: Option<LineFields> = None;
        let visible_double = c.double_mode != 0
            && (c.double_width > 0 || c.double_left_width > 0 || c.double_right_width > 0);
        if visible_double {
            let into_main = !(main.dashed || main.visible());
            let mut spare = LineFields::empty();
            let fields = self.double_border_fields(c);
            let target = if into_main { &mut main } else { &mut spare };
            apply_double_border(target, c, fields);
            if !into_main {
                double = Some(spare);
            }
        }
        if c.double_flags & 2 != 0 {
            self.warn(format!(
                "In line symbol {code}: unsupported line style 'DoubleBackgroundColorOn'."
            ));
        }

        if double.is_none() && framing.is_none() {
            return Ok((main.to_xml(), 2));
        }
        let parts: Vec<LineFields> = [Some(main), double, framing]
            .into_iter()
            .flatten()
            .collect();
        let parts_xml: String = parts
            .iter()
            .map(|p| {
                format!(
                    "<part private=\"true\"><symbol type=\"2\" code=\"\">{}</symbol></part>",
                    p.to_xml()
                )
            })
            .collect();
        Ok((
            format!(
                "<combined_symbol parts=\"{}\">{parts_xml}</combined_symbol>",
                parts.len()
            ),
            16,
        ))
    }

    /// The stroke itself: width, colour, ends and dash pattern.
    ///
    /// After Mapper's `importLineSymbolBase`.
    fn line_base(&mut self, c: &LineCommon, code: &str) -> LineFields {
        let mut line = LineFields::empty();
        line.line_width = c.line_width * 10;
        line.color = if line.line_width != 0 {
            self.color_ref(c.line_color)
        } else {
            "-1".to_string()
        };

        let (join, cap) = match c.line_style {
            0 => (0, 0),
            1 => (2, 1),
            2 => (0, 3),
            3 => (2, 3),
            4 => (1, 0),
            6 => (1, 3),
            other => {
                self.warn(format!(
                    "In line symbol {code}: unsupported line style '{other}'."
                ));
                (0, 0)
            }
        };
        line.join_style = join;
        line.cap_style = cap;
        line.start_offset = c.dist_from_start.max(0) * 10;
        line.end_offset = c.dist_from_end.max(0) * 10;
        // OCAD always rounds the joins of a line whose ends are pointed.
        if line.cap_style == 3 {
            line.join_style = 2;
        }

        if c.main_gap != 0 || c.sec_gap != 0 {
            if c.main_length == 0 {
                self.warn(format!(
                    "In line symbol {code}: the dash pattern cannot be imported correctly."
                ));
            } else if c.sec_gap != 0 && c.main_gap == 0 {
                // No main gap at all: the dashes are split by the secondary
                // gap instead, and the main length spans the pair.
                line.dashed = true;
                line.dash_length = (c.main_length - c.sec_gap) * 10;
                line.break_length = c.sec_gap * 10;
            } else {
                line.dashed = true;
                line.dash_length = c.main_length * 10;
                line.break_length = c.main_gap * 10;
                // An end dash noticeably shorter than the rest is the XML's
                // half-length outer dashes.
                if c.end_length != 0
                    && c.end_length != c.main_length
                    && (c.end_length as f64) / (c.main_length as f64) <= 0.75
                {
                    line.half_outer_dashes = true;
                }
                if c.sec_gap != 0 {
                    line.dashes_in_group = 2;
                    line.in_group_break_length = c.sec_gap * 10;
                    line.dash_length = (line.dash_length - line.in_group_break_length) / 2;
                }
            }
        } else {
            line.segment_length = c.main_length * 10;
            line.end_length = c.end_length * 10;
        }
        line
    }

    /// The glyphs placed along a line: in its gaps, at its corners, at each
    /// end. After Mapper's `setupLineSymbolPointSymbols`.
    fn line_sub_symbols(
        &mut self,
        line: &mut LineFields,
        c: &LineCommon,
        elements_off: usize,
    ) -> Rd<()> {
        let block = |this: &mut Self, unit_offset: usize, size: usize| -> Rd<String> {
            if size == 0 {
                return Ok(String::new());
            }
            let body = this.point_symbol_xml(size, elements_off + unit_offset * 8, true)?;
            Ok(format!("<symbol type=\"1\" code=\"\">{body}</symbol>"))
        };

        // With no main gap the roles of the two glyph sets swap over: what
        // would sit in a gap sits in the middle of a dash group instead.
        let gaps_swapped = c.sec_gap != 0 && c.main_gap == 0 && c.main_length != 0;
        if c.primary_data_size > 0 {
            line.mid_symbol_placement = if gaps_swapped { 1 } else { 2 };
            line.mid_symbols_per_spot = c.num_prim_sym;
            line.mid_symbol_distance = c.prim_sym_dist * 10;
            line.show_at_least_one_symbol = true;
            line.mid_symbol = block(self, 0, c.primary_data_size)?;
            if c.secondary_data_size > 0 {
                self.warn("Skipped secondary point symbol.".to_string());
            }
        } else if c.secondary_data_size > 0 {
            line.mid_symbol_placement = if gaps_swapped { 2 } else { 1 };
            line.mid_symbols_per_spot = 1;
            line.show_at_least_one_symbol = true;
            line.mid_symbol = block(self, 0, c.secondary_data_size)?;
        }

        let mut unit = c.primary_data_size + c.secondary_data_size;
        if c.corner_data_size > 0 {
            line.dash_symbol = block(self, unit, c.corner_data_size)?;
            unit += c.corner_data_size;
        }
        if c.start_data_size > 0 {
            line.start_symbol = block(self, unit, c.start_data_size)?;
            unit += c.start_data_size;
        }
        if c.end_data_size > 0 {
            line.end_symbol = block(self, unit, c.end_data_size)?;
        }
        Ok(())
    }

    /// The colours of a double line, looked up before its fields are set, so
    /// that the borrow of the line being written into stands alone.
    fn double_border_fields(&mut self, c: &LineCommon) -> (String, String, String) {
        let fill = if c.double_width != 0 && c.double_flags & 1 != 0 {
            self.color_ref(c.double_color)
        } else {
            "-1".to_string()
        };
        let left = if c.double_left_width != 0 {
            self.color_ref(c.double_left_color)
        } else {
            "-1".to_string()
        };
        let right = if c.double_right_width != 0 {
            self.color_ref(c.double_right_color)
        } else {
            "-1".to_string()
        };
        (fill, left, right)
    }
}

/// Turns an OCAD double line into a stroke with two borders.
///
/// After Mapper's `setupLineSymbolDoubleBorder`. The fill between the borders
/// becomes the stroke itself, and the borders become the XML's own border
/// fields, shifted out by half their width to sit either side of it.
fn apply_double_border(line: &mut LineFields, c: &LineCommon, colors: (String, String, String)) {
    let (fill_color, left_color, right_color) = colors;
    line.line_width = c.double_width * 10;
    line.color = fill_color;
    line.cap_style = 0;
    line.join_style = 1;

    let mut left = BorderFields {
        color: left_color,
        width: c.double_left_width * 10,
        shift: c.double_left_width * 10 / 2,
        dashed: false,
        dash_length: 0,
        break_length: 0,
    };
    let mut right = BorderFields {
        color: right_color,
        width: c.double_right_width * 10,
        shift: c.double_right_width * 10 / 2,
        dashed: false,
        dash_length: 0,
        break_length: 0,
    };

    // Which of the two borders is dashed, and whether the fill goes with
    // them, is what the mode says.
    if c.double_gap > 0 && c.double_mode != 1 {
        left.dashed = true;
        left.dash_length = c.double_length * 10;
        left.break_length = c.double_gap * 10;
        if c.double_mode != 2 {
            right.dashed = true;
            right.dash_length = left.dash_length;
            right.break_length = left.break_length;
            if c.double_mode == 4 {
                line.dashed = true;
                line.dashes_in_group = 1;
                line.dash_length = left.dash_length;
                line.break_length = left.break_length;
                line.half_outer_dashes = false;
            }
        }
    }
    line.borders = vec![left, right];
}

// ---------------------------------------------------------------------------
// Area and text symbols

impl Import<'_> {
    /// The fill of an area, and the hatching or the pattern of glyphs over it.
    ///
    /// After Mapper's `setupAreaSymbolCommon`.
    fn area_symbol_xml(
        &mut self,
        fill_on: bool,
        common_off: usize,
        data_size: usize,
        elements_off: usize,
    ) -> Rd<String> {
        let fill_color = i64::from(self.u16(common_off)?);
        let hatch_mode = self.u16(common_off + 2)?;
        let hatch_color = i64::from(self.u16(common_off + 4)?);
        let hatch_line_width = i64::from(self.u16(common_off + 6)?);
        let hatch_dist = i64::from(self.u16(common_off + 8)?);
        let hatch_angle_1 = i32::from(self.i16(common_off + 10)?);
        let hatch_angle_2 = i32::from(self.i16(common_off + 12)?);
        let structure_mode = self.u8(common_off + 16)?;
        let structure_width = i64::from(self.u16(common_off + 18)?);
        let structure_height = i64::from(self.u16(common_off + 20)?);
        let structure_angle = i32::from(self.i16(common_off + 22)?);

        let mut patterns: Vec<String> = Vec::new();

        if hatch_mode != 0 && hatch_line_width != 0 {
            let line_width = hatch_line_width * 10;
            // Until version 8 the spacing was measured between the lines
            // rather than from one to the next.
            let line_spacing = hatch_dist * 10
                + if self.family == Family::V8 {
                    line_width
                } else {
                    0
                };
            let color = self.color_ref(hatch_color);
            let hatch = |angle: i32| {
                format!(
                    "<pattern type=\"1\" color=\"{color}\" line_width=\"{line_width}\" line_spacing=\"{line_spacing}\" line_offset=\"0\" angle=\"{}\" rotatable=\"true\"/>",
                    num(angle_radians(angle))
                )
            };
            patterns.push(hatch(hatch_angle_1));
            if hatch_mode == 2 {
                patterns.push(hatch(hatch_angle_2));
            }
        }

        if structure_mode != 0 && structure_height != 0 && structure_width != 0 && data_size != 0 {
            let body = self.point_symbol_xml(data_size, elements_off, true)?;
            let point_xml = format!("<symbol type=\"1\" code=\"\">{body}</symbol>");
            let angle = num(angle_radians(structure_angle));
            let point_distance = structure_width * 10;
            let mut line_spacing = structure_height * 10;
            let mut line_offset = 0;
            let mut offset_along_line = 0;
            // Shifted rows are two patterns of aligned rows, twice as far
            // apart, the second offset by half a cell each way -- which is
            // the only way the XML can describe a staggered grid.
            if structure_mode == 2 {
                line_spacing *= 2;
                patterns.push(format!(
                    "<pattern type=\"2\" color=\"-1\" point_distance=\"{point_distance}\" line_spacing=\"{line_spacing}\" line_offset=\"0\" offset_along_line=\"0\" angle=\"{angle}\" rotatable=\"true\">{point_xml}</pattern>"
                ));
                line_offset = line_spacing / 2;
                offset_along_line = point_distance / 2;
            }
            patterns.push(format!(
                "<pattern type=\"2\" color=\"-1\" point_distance=\"{point_distance}\" line_spacing=\"{line_spacing}\" line_offset=\"{line_offset}\" offset_along_line=\"{offset_along_line}\" angle=\"{angle}\" rotatable=\"true\">{point_xml}</pattern>"
            ));
        }

        let inner_color = if fill_on {
            self.color_ref(fill_color)
        } else {
            "-1".to_string()
        };
        Ok(format!(
            "<area_symbol inner_color=\"{inner_color}\" min_area=\"0\" patterns=\"{}\">{}</area_symbol>",
            patterns.len(),
            patterns.concat()
        ))
    }
}

/// A text symbol, and the alignment its objects inherit from it.
struct TextSymbol {
    xml: String,
    h_align: u8,
    v_align: u8,
}

impl Import<'_> {
    fn text_symbol_xml(
        &mut self,
        font_name: &str,
        basic_off: usize,
        has_special: bool,
        code: &str,
        name: &str,
    ) -> Rd<TextSymbol> {
        let color = i64::from(self.u16(basic_off)?);
        // Tenths of a typographic point.
        let font_size_ocd = f64::from(self.u16(basic_off + 2)?);
        let font_weight = self.u16(basic_off + 4)?;
        let italic = self.u8(basic_off + 6)? != 0;
        let alignment = self.u16(basic_off + 12)?;

        // The XML measures a font in 1/1000 mm: a tenth of a point is
        // 25.4/72 mm.
        let font_size = round_half_up(font_size_ocd * 100.0 * 25.4 / 72.0);

        // Line spacing is a percentage of the font size in OCAD, and a
        // multiple of it in the XML.
        let mut line_spacing = 1.0;
        if has_special {
            let ls = self.u16(basic_off + 14)?;
            if ls > 0 {
                line_spacing = f64::from(ls) / 100.0;
            }
        }

        let h_align = match alignment & 0x03 {
            0 => 0,
            2 => 2,
            3 => {
                self.warn(format!(
                    "In text symbol {code} '{name}': justified alignment is not supported."
                ));
                1
            }
            _ => 1,
        };
        // Where the anchor sits vertically is only said from version 10 on;
        // before that it is always the baseline.
        let v_align = if self.version >= 10 {
            match alignment & 0x0c {
                0x08 => 1,
                0x04 => 2,
                _ => 0,
            }
        } else {
            0
        };

        let color = self.color_ref(color);
        let bold = if font_weight >= 550 {
            " bold=\"true\""
        } else {
            ""
        };
        let italic = if italic { " italic=\"true\"" } else { "" };
        Ok(TextSymbol {
            xml: format!(
                "<text_symbol rotatable=\"true\"><font family=\"{}\" size=\"{font_size}\"{bold}{italic}/><text color=\"{color}\" line_spacing=\"{}\"/></text_symbol>",
                xml_escape(font_name),
                num(line_spacing)
            ),
            h_align,
            v_align,
        })
    }
}

/// An OCAD angle -- tenths of a degree, counterclockwise -- in radians.
fn angle_radians(ocd_angle: i32) -> f64 {
    f64::from((ocd_angle + 3600).rem_euclid(3600)) / 10.0 * std::f64::consts::PI / 180.0
}

// ---------------------------------------------------------------------------
// Objects

impl Import<'_> {
    /// Walks the chain of object blocks and translates every live object.
    fn import_objects(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut skipped = 0usize;
        let entry_size = if self.family == Family::V8 { 24 } else { 40 };
        let mut block_pos = self.u32(12).unwrap_or(0) as usize;
        let mut seen = HashSet::new();
        while block_pos != 0 && self.in_bounds(block_pos, 4 + 256 * entry_size) {
            if !seen.insert(block_pos) {
                break;
            }
            for i in 0..256 {
                let e = block_pos + 4 + i * entry_size;
                let pos = self.u32(e + 16).unwrap_or(0) as usize;
                if pos == 0 {
                    continue;
                }
                // A deleted object is left in place with its symbol cleared;
                // from version 9 an undone one is marked in its status.
                let deleted = if self.family == Family::V8 {
                    self.i16(e + 22).map(|s| s == 0).unwrap_or(true)
                } else {
                    match (self.i32(e + 24), self.u8(e + 30)) {
                        (Ok(symbol), Ok(status)) => symbol == 0 || status == 0 || status == 3,
                        _ => true,
                    }
                };
                if deleted {
                    continue;
                }
                match self.object_xml(pos) {
                    Ok(Some(xml)) => out.push(xml),
                    Ok(None) => skipped += 1,
                    Err(err) => self.warn(format!(
                        "Skipping unreadable object at file position {pos}: {err}"
                    )),
                }
            }
            block_pos = self.u32(block_pos).unwrap_or(0) as usize;
        }
        if skipped > 0 {
            self.warn(format!(
                "Skipped {skipped} object(s) referencing missing or unsupported symbols."
            ));
        }
        out
    }

    fn object_xml(&mut self, pos: usize) -> Rd<Option<String>> {
        let f = self.family;
        let symbol_num: i64;
        let angle: i32;
        let num_items: usize;
        let num_text: usize;
        let mut unicode = 1u8;
        let coords_off: usize;
        match f {
            Family::V8 => {
                if !self.in_bounds(pos, 32) {
                    return Ok(None);
                }
                symbol_num = i64::from(self.i16(pos)?);
                unicode = self.u8(pos + 3)?;
                num_items = self.u16(pos + 4)? as usize;
                num_text = self.u16(pos + 6)? as usize;
                angle = i32::from(self.i16(pos + 8)?);
                coords_off = pos + 32;
            }
            Family::V12 => {
                if !self.in_bounds(pos, 56) {
                    return Ok(None);
                }
                symbol_num = i64::from(self.i32(pos)?);
                angle = i32::from(self.i16(pos + 6)?);
                num_items = self.u32(pos + 44)? as usize;
                num_text = self.u16(pos + 48)? as usize;
                coords_off = pos + 56;
            }
            _ => {
                if !self.in_bounds(pos, 40) {
                    return Ok(None);
                }
                symbol_num = i64::from(self.i32(pos)?);
                angle = i32::from(self.i16(pos + 6)?);
                num_items = self.u32(pos + 8)? as usize;
                num_text = self.u16(pos + 12)? as usize;
                coords_off = pos + 40;
            }
        }
        if !self.in_bounds(coords_off, num_items * 8) {
            return Ok(None);
        }

        // A negative number is OCAD's own: an imported image or a graphic,
        // drawn with no symbol this map defines.
        let Some(&at) = (if symbol_num >= 0 {
            self.symbol_by_number.get(&symbol_num)
        } else {
            None
        }) else {
            return Ok(None);
        };
        let (symbol_id, object_type, omap_type, rect_corner_radius, sym_h_align, sym_v_align) = {
            let sym = &self.symbols[at];
            (
                sym.id,
                sym.object_type,
                sym.omap_type,
                sym.rect_corner_radius,
                sym.h_align,
                sym.v_align,
            )
        };

        let rotation = angle_radians(angle);
        let rotation_attr = if rotation != 0.0 {
            format!(" rotation=\"{}\"", num(rotation))
        } else {
            String::new()
        };

        if object_type == 0 {
            let p = self.coord(coords_off)?;
            return Ok(Some(format!(
                "<object type=\"0\" symbol=\"{symbol_id}\"{rotation_attr}><coords count=\"1\">{} {};</coords></object>",
                p.x, p.y
            )));
        }

        if object_type == 4 {
            let text = self.object_text(coords_off, num_items, num_text, unicode)?;
            if text.is_empty() {
                return Ok(None);
            }
            let (x, y, h_align, v_align) = if num_items == 4 {
                // A text box, which the XML has no equivalent of: the text is
                // anchored at the middle of the box instead, and its
                // alignment goes with it.
                let bl = self.coord(coords_off)?;
                let tr = self.coord(coords_off + 16)?;
                (
                    round_half_up((bl.x + tr.x) as f64 / 2.0) as i64,
                    round_half_up((bl.y + tr.y) as f64 / 2.0) as i64,
                    1,
                    2,
                )
            } else {
                let p = self.coord(coords_off)?;
                (p.x, p.y, sym_h_align, sym_v_align)
            };
            return Ok(Some(format!(
                "<object type=\"4\" symbol=\"{symbol_id}\"{rotation_attr} h_align=\"{h_align}\" v_align=\"{v_align}\"><coords count=\"1\">{x} {y};</coords><text>{}</text></object>",
                xml_escape(&text)
            )));
        }

        if rect_corner_radius.is_some() && (num_items == 4 || num_items == 5) {
            return Ok(Some(self.rectangle_object_xml(
                symbol_id,
                rect_corner_radius.unwrap_or(0),
                coords_off,
            )?));
        }

        // Only a real area closes its paths. A combined symbol built out of
        // an OCAD line -- for its framing or its border -- is still a line.
        let is_area = omap_type == 4;
        let coords = self.path_coord_string(coords_off, num_items, is_area)?;
        // A rotation on a path object can only mean the fill pattern's.
        let pattern = if rotation != 0.0 {
            format!(
                "<pattern rotation=\"{}\"><coord x=\"0\" y=\"0\"/></pattern>",
                num(rotation)
            )
        } else {
            String::new()
        };
        Ok(Some(format!(
            "<object type=\"1\" symbol=\"{symbol_id}\"><coords count=\"{num_items}\">{coords}</coords>{pattern}</object>"
        )))
    }

    /// The text of a text object, which follows its coordinates.
    fn object_text(
        &self,
        coords_off: usize,
        num_items: usize,
        num_text: usize,
        unicode: u8,
    ) -> Rd<String> {
        let start = coords_off + num_items * 8;
        let size = num_text * 8;
        if num_text == 0 || !self.in_bounds(start, size) {
            return Ok(String::new());
        }
        let raw = self.slice(start, size)?;
        let text = if self.family == Family::V8 && unicode == 0 {
            decode_cp1252(until_nul(raw))
        } else {
            let mut end = 0;
            while end + 1 < raw.len() && (raw[end] != 0 || raw[end + 1] != 0) {
                end += 2;
            }
            decode_utf16le(&raw[..end])
        };
        // OCAD leads a text object's text with a line break of its own.
        let text = text.strip_prefix("\r\n").unwrap_or(&text).to_string();
        Ok(normalize_newlines(&text))
    }

    /// The coordinates of a path, with the flags the XML uses for them.
    ///
    /// After Mapper's `fillPathCoords`. Two things happen here beyond
    /// translating the flag bits: a subpath which ends where it began is put
    /// into the closed form the XML expects, and a bezier which would be cut
    /// short by the end of its subpath has its curve flag taken off, since
    /// three coordinates are needed after one and there are not three left.
    fn build_path_coords(
        &self,
        coords_off: usize,
        num_points: usize,
        is_area: bool,
    ) -> Rd<Vec<(i64, i64, i32)>> {
        let mut coords: Vec<(i64, i64, i32)> = Vec::with_capacity(num_points + 1);
        for i in 0..num_points {
            let p = self.coord(coords_off + i * 8)?;
            coords.push((p.x, p.y, 0));
            if p.xf & 0x01 != 0 && i > 0 {
                coords[i - 1].2 |= coord_flag::CURVE_START;
            }
            if p.yf & 0x08 != 0 || p.yf & 0x01 != 0 {
                coords[i].2 |= coord_flag::DASH_POINT;
            }
            if p.yf & 0x02 != 0 && i > 1 && is_area {
                set_path_hole_point(&mut coords, i - 1);
            }
        }

        let mut start = 0usize;
        let mut i = 0usize;
        while i < coords.len() {
            let last = i == coords.len() - 1;
            if coords[i].2 & coord_flag::HOLE_POINT == 0 && !last {
                i += 1;
                continue;
            }
            let closing = (
                coords[start].0,
                coords[start].1,
                (coords[start].2 & !(coord_flag::CURVE_START | coord_flag::HOLE_POINT))
                    | (coords[i].2 & coord_flag::HOLE_POINT)
                    | coord_flag::CLOSE_POINT,
            );
            if coords[i].0 == closing.0 && coords[i].1 == closing.1 {
                coords[i].2 = closing.2;
            } else if is_area {
                coords.insert(i + 1, closing);
                coords[i].2 &= !coord_flag::HOLE_POINT;
                i += 1;
            }
            if i - start >= 2 {
                coords[i - 2].2 &= !coord_flag::CURVE_START;
            }
            if i - start >= 1 {
                coords[i - 1].2 &= !coord_flag::CURVE_START;
            }
            start = i + 1;
            i += 1;
        }
        Ok(coords)
    }

    fn path_coord_string(&self, coords_off: usize, num_points: usize, is_area: bool) -> Rd<String> {
        Ok(coord_string(
            &self.build_path_coords(coords_off, num_points, is_area)?,
        ))
    }

    /// The outline of a rectangle object, corners and all.
    ///
    /// After Mapper's `importRectangleObject`. The cell grid such a symbol can
    /// carry is not imported; only the rectangle itself is.
    fn rectangle_object_xml(&self, symbol_id: usize, radius: i64, coords_off: usize) -> Rd<String> {
        let bl = self.coord(coords_off)?;
        let br = self.coord(coords_off + 8)?;
        let tr = self.coord(coords_off + 16)?;
        let tl = self.coord(coords_off + 24)?;

        let mut coords: Vec<(i64, i64, i32)> = Vec::new();
        if radius == 0 {
            coords.push((tl.x, tl.y, 0));
            coords.push((tr.x, tr.y, 0));
            coords.push((br.x, br.y, 0));
            coords.push((bl.x, bl.y, 0));
        } else {
            let unit = |dx: f64, dy: f64| {
                let len = dx.hypot(dy);
                let len = if len == 0.0 { 1.0 } else { len };
                (dx / len, dy / len)
            };
            let right = unit((tr.x - tl.x) as f64, (tr.y - tl.y) as f64);
            let down = unit((bl.x - tl.x) as f64, (bl.y - tl.y) as f64);
            let left = (-right.0, -right.1);
            let up = (-down.0, -down.1);
            let r = radius as f64;
            // The control points of a quarter circle, at the distance from
            // the corner which makes the curve meet the sides smoothly.
            let handle = (1.0 - BEZIER_KAPPA) * r;
            let mut corner = |c: &OcdCoord, in_dir: (f64, f64), out_dir: (f64, f64)| {
                let (cx, cy) = (c.x as f64, c.y as f64);
                let at = |dir: (f64, f64), d: f64| {
                    (
                        round_half_up(cx + dir.0 * d) as i64,
                        round_half_up(cy + dir.1 * d) as i64,
                    )
                };
                let (x0, y0) = at((-in_dir.0, -in_dir.1), r);
                let (x1, y1) = at((-in_dir.0, -in_dir.1), handle);
                let (x2, y2) = at(out_dir, handle);
                let (x3, y3) = at(out_dir, r);
                coords.push((x0, y0, coord_flag::CURVE_START));
                coords.push((x1, y1, 0));
                coords.push((x2, y2, 0));
                coords.push((x3, y3, 0));
            };
            corner(&tr, right, down);
            corner(&br, down, left);
            corner(&bl, left, up);
            corner(&tl, up, right);
        }
        coords.push((coords[0].0, coords[0].1, coord_flag::CLOSE_POINT));
        Ok(format!(
            "<object type=\"1\" symbol=\"{symbol_id}\"><coords count=\"{}\">{}</coords></object>",
            coords.len(),
            coord_string(&coords)
        ))
    }
}

/// Marks the end of one ring of an area, unless it would fall inside a bezier
/// -- where the three coordinates after a curve start are its control points
/// and its end, and none of them can be the end of anything else.
///
/// After Mapper's `setPathHolePoint`.
fn set_path_hole_point(coords: &mut [(i64, i64, i32)], pos: usize) {
    if pos >= 1 && coords[pos].2 & coord_flag::CURVE_START != 0 {
        return;
    }
    if pos >= 2 && coords[pos - 1].2 & coord_flag::CURVE_START != 0 {
        return;
    }
    if pos >= 3 && coords[pos - 2].2 & coord_flag::CURVE_START != 0 {
        return;
    }
    if pos > 0 {
        coords[pos].2 |= coord_flag::HOLE_POINT;
    }
}

/// The coordinate list of an object, as the XML writes one.
fn coord_string(coords: &[(i64, i64, i32)]) -> String {
    let mut out = String::with_capacity(coords.len() * 16);
    for &(x, y, flags) in coords {
        if flags != 0 {
            out.push_str(&format!("{x} {y} {flags};"));
        } else {
            out.push_str(&format!("{x} {y};"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Georeferencing and notes

/// The EPSG code a combined grid and zone code names, where one can be worked
/// out.
///
/// `None` where the grid is local coordinates -- which is not a failure, the
/// map simply has no projection -- and where the grid is one this does not
/// know. UTM and the German Gauss-Krueger zones are computed; everything else
/// comes from the table in [`crate::ocd_crs`].
pub fn epsg_from_grid_zone(grid_zone: i32) -> Option<u32> {
    let grid_id = (grid_zone / 1000).abs();
    let zone_id = (grid_zone % 1000).abs() * if grid_zone < 0 { -1 } else { 1 };
    match grid_id {
        // Local coordinates: no projection to name.
        1 => None,
        // UTM, north and south.
        2 if (1..=60).contains(&zone_id) => Some(32600 + zone_id as u32),
        2 if (-60..=-1).contains(&zone_id) => Some((32700 - zone_id) as u32),
        8 if (2..=5).contains(&zone_id) => Some(31464 + zone_id as u32),
        _ => ocd_crs::lookup(grid_zone),
    }
}

impl Import<'_> {
    /// The map's scale, its reference point and its projection.
    fn import_georeferencing(&mut self) -> String {
        let mut scale = 0f64;
        let mut ref_x = 0f64;
        let mut ref_y = 0f64;
        let mut grivation: Option<f64> = None;
        let mut epsg = 0u32;

        if self.family == Family::V8 {
            let setup_pos = self.u32(16).unwrap_or(0) as usize;
            let setup_size = self.u32(20).unwrap_or(0) as usize;
            if setup_pos != 0 && setup_size >= 56 && self.in_bounds(setup_pos, 56) {
                scale = self.f64(setup_pos + 24).map(round_half_up).unwrap_or(0.0);
                ref_x = self.f64(setup_pos + 32).unwrap_or(0.0);
                ref_y = self.f64(setup_pos + 40).unwrap_or(0.0);
                let a = self.f64(setup_pos + 48).unwrap_or(0.0);
                grivation = Some(if a.is_finite() { a } else { 0.0 });
            }
        } else if let Some(s) = self.param_strings(1039).first() {
            // "ScalePar": the scale, the offset of the map on the ground, the
            // grid it is on and the angle between its north and the grid's.
            let parsed = ParamString::parse(s);
            let mut grid_zone = 0i32;
            let mut real_world = false;
            if let Some(v) = parsed.number('m') {
                scale = round_half_up(v);
            }
            if let Some(v) = parsed.number('x') {
                ref_x = v;
            }
            if let Some(v) = parsed.number('y') {
                ref_y = v;
            }
            if let Some(v) = parsed.number('a') {
                grivation = Some(v);
            }
            if let Some(v) = parsed.number('i') {
                grid_zone = round_half_up(v) as i32;
            }
            if let Some(v) = parsed.number('r') {
                real_world = round_half_up(v) != 0.0;
            }
            if real_world {
                match epsg_from_grid_zone(grid_zone) {
                    Some(code) => epsg = code,
                    None if (grid_zone / 1000).abs() == 1 => {}
                    None => self.warn(format!(
                        "Could not resolve the coordinate reference system '{grid_zone}'; importing without EPSG code."
                    )),
                }
            }
        }

        if scale == 0.0 {
            self.warn("No map scale found in the file; assuming 1:10000.".to_string());
            scale = 10000.0;
        }

        let grivation_attr = match grivation {
            Some(g) => format!(" grivation=\"{}\"", num(g)),
            None => String::new(),
        };
        let ref_point = format!("<ref_point x=\"{}\" y=\"{}\"/>", num(ref_x), num(ref_y));
        let crs = if epsg != 0 {
            format!("<projected_crs id=\"EPSG\"><parameter>{epsg}</parameter>{ref_point}</projected_crs>")
        } else {
            format!("<projected_crs id=\"Local\">{ref_point}</projected_crs>")
        };
        format!(
            "<georeferencing scale=\"{}\"{grivation_attr}>{crs}</georeferencing>\n",
            num(scale)
        )
    }

    /// Whatever the mapper wrote about the map.
    fn import_notes(&self) -> String {
        if self.family == Family::V8 {
            let pos = self.u32(24).unwrap_or(0) as usize;
            let size = self.u32(28).unwrap_or(0) as usize;
            if pos == 0 || size == 0 || !self.in_bounds(pos, size) {
                return String::new();
            }
            let Ok(raw) = self.slice(pos, size) else {
                return String::new();
            };
            return normalize_newlines(&decode_cp1252(until_nul(raw)));
        }
        let mut notes = self.param_strings(11);
        notes.extend(self.param_strings(1061));
        normalize_newlines(&notes.concat())
    }
}
