//! Reads a map in the native XML format of OpenOrienteering Mapper.
//!
//! Ported from `xml_reader.cpp`. Both the ".omap" and the ".xmap" file name
//! suffixes denote this format; they differ in formatting only.

use std::path::Path;

use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::map::*;

/// A normalized XML event: self-closing (`Empty`) elements are presented as
/// a `Start` immediately followed by a synthetic `End`, mirroring
/// `QXmlStreamReader`'s token stream, so the rest of the reader does not have
/// to special-case them. Whitespace-only text is dropped, except where a
/// dedicated raw text reader is used (element content such as `<text>`).
enum Ev<'a> {
    Start(BytesStart<'a>),
    End,
    Text(String),
    Eof,
}

struct Xml<'a> {
    reader: Reader<&'a [u8]>,
    pending_end: bool,
}

impl<'a> Xml<'a> {
    fn new(content: &'a str) -> Xml<'a> {
        Xml {
            reader: Reader::from_str(content),
            pending_end: false,
        }
    }

    fn next(&mut self) -> Result<Ev<'a>, String> {
        if self.pending_end {
            self.pending_end = false;
            return Ok(Ev::End);
        }
        loop {
            match self.reader.read_event().map_err(|e| e.to_string())? {
                Event::Start(e) => return Ok(Ev::Start(e)),
                Event::Empty(e) => {
                    self.pending_end = true;
                    return Ok(Ev::Start(e));
                }
                Event::End(_) => return Ok(Ev::End),
                Event::Text(t) => {
                    let s = t.unescape().map_err(|e| e.to_string())?.into_owned();
                    if s.trim().is_empty() {
                        continue;
                    }
                    return Ok(Ev::Text(s));
                }
                Event::CData(t) => {
                    let s = String::from_utf8_lossy(&t.into_inner()).into_owned();
                    if s.trim().is_empty() {
                        continue;
                    }
                    return Ok(Ev::Text(s));
                }
                Event::Eof => return Ok(Ev::Eof),
                _ => continue,
            }
        }
    }

    /// Skips to the matching end of an element just entered via `Ev::Start`.
    fn skip_current(&mut self) -> Result<(), String> {
        let mut depth = 0i32;
        loop {
            match self.next()? {
                Ev::Start(_) => depth += 1,
                Ev::End => {
                    if depth == 0 {
                        return Ok(());
                    }
                    depth -= 1;
                }
                Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    /// Reads all text content of the current element, without dropping
    /// whitespace-only runs (unlike `next()`), for text objects. Mirrors
    /// `QXmlStreamReader::readElementText()`.
    fn read_text_content(&mut self) -> Result<String, String> {
        let mut text = String::new();
        loop {
            match self.reader.read_event().map_err(|e| e.to_string())? {
                Event::Text(t) => text.push_str(&t.unescape().map_err(|e| e.to_string())?),
                Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t.into_inner())),
                Event::Start(_) | Event::Empty(_) => {
                    // Not expected in this format; skip the unexpected child
                    // rather than misreading its text as our own.
                    let mut depth = 0i32;
                    loop {
                        match self.reader.read_event().map_err(|e| e.to_string())? {
                            Event::Start(_) => depth += 1,
                            Event::End(_) => {
                                if depth == 0 {
                                    break;
                                }
                                depth -= 1;
                            }
                            Event::Eof => break,
                            _ => {}
                        }
                    }
                }
                Event::End(_) => return Ok(text),
                Event::Eof => return Ok(text),
                _ => {}
            }
        }
    }
}

fn local_name(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

fn find_attr<'a>(e: &'a BytesStart, name: &str) -> Option<Attribute<'a>> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name.as_bytes())
}

fn has_attr(e: &BytesStart, name: &str) -> bool {
    find_attr(e, name).is_some()
}

fn attr_str(e: &BytesStart, name: &str) -> Option<String> {
    find_attr(e, name).and_then(|a| a.unescape_value().ok().map(|c| c.into_owned()))
}

fn attr_int(e: &BytesStart, name: &str, fallback: i32) -> i32 {
    match attr_str(e, name) {
        Some(s) if !s.is_empty() => s.trim().parse::<i32>().unwrap_or(fallback),
        _ => fallback,
    }
}

fn attr_double(e: &BytesStart, name: &str, fallback: f64) -> f64 {
    match attr_str(e, name) {
        Some(s) if !s.is_empty() => s.trim().parse::<f64>().unwrap_or(fallback),
        _ => fallback,
    }
}

/// Reads a length attribute given in native units, and returns it in mm.
fn attr_length(e: &BytesStart, name: &str, fallback: f64) -> f64 {
    MM_PER_UNIT * attr_double(e, name, fallback / MM_PER_UNIT)
}

fn attr_bool(e: &BytesStart, name: &str, fallback: bool) -> bool {
    match attr_str(e, name) {
        Some(s) if !s.is_empty() => s == "true",
        _ => fallback,
    }
}

/// Parses one "x y[ flags];" coordinate record, advancing the cursor past it.
fn parse_coord(cursor: &mut &str) -> Option<Coord> {
    fn take_token<'a>(cursor: &mut &'a str) -> &'a str {
        let s = cursor.trim_start();
        let end = s
            .find(|c: char| c.is_whitespace() || c == ';')
            .unwrap_or(s.len());
        let (tok, rest) = s.split_at(end);
        *cursor = rest;
        tok
    }

    let x: i32 = take_token(cursor).parse().ok()?;
    let y: i32 = take_token(cursor).parse().ok()?;
    let flags_tok = take_token(cursor);
    let flags: i32 = if flags_tok.is_empty() {
        0
    } else {
        flags_tok.parse().unwrap_or(0)
    };
    *cursor = cursor.trim_start_matches(|c: char| c.is_whitespace() || c == ';');
    Some(Coord::new(MM_PER_UNIT * x as f64, MM_PER_UNIT * y as f64, flags))
}

/// Converts CMYK to RGB the way `QColor::toRgb()` does for a color created
/// with `QColor::fromCmykF`, in the same float precision. This approximates
/// Qt's internal 16-bit fixed-point storage with a direct float computation
/// and can differ from Qt by at most one 8-bit level in rare rounding cases.
fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> (f32, f32, f32) {
    let r = 1.0 - (c * (1.0 - k) + k);
    let g = 1.0 - (m * (1.0 - k) + k);
    let b = 1.0 - (y * (1.0 - k) + k);
    (r, g, b)
}

fn point_mut(s: &mut Symbol) -> &mut PointSymbol {
    match s {
        Symbol::Point(p) => p,
        _ => unreachable!(),
    }
}
fn line_mut(s: &mut Symbol) -> &mut LineSymbol {
    match s {
        Symbol::Line(l) => l,
        _ => unreachable!(),
    }
}
fn area_mut(s: &mut Symbol) -> &mut AreaSymbol {
    match s {
        Symbol::Area(a) => a,
        _ => unreachable!(),
    }
}
fn text_mut(s: &mut Symbol) -> &mut TextSymbol {
    match s {
        Symbol::Text(t) => t,
        _ => unreachable!(),
    }
}
fn combined_mut(s: &mut Symbol) -> &mut CombinedSymbol {
    match s {
        Symbol::Combined(c) => c,
        _ => unreachable!(),
    }
}

struct XmlMapReader<'a> {
    xml: Xml<'a>,
    map: Map,
    warnings: Vec<String>,
    version: i32,
}

impl<'a> XmlMapReader<'a> {
    fn new(content: &'a str) -> XmlMapReader<'a> {
        XmlMapReader {
            xml: Xml::new(content),
            map: Map::new(),
            warnings: Vec::new(),
            version: 0,
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// Reads the children of the current element, resolving barrier elements
    /// (which wrap sections older versions of Mapper cannot read) by
    /// flattening them transparently.
    fn read_children(
        &mut self,
        handler: &mut dyn FnMut(&mut Self, &str, &BytesStart) -> Result<bool, String>,
    ) -> Result<(), String> {
        loop {
            match self.xml.next()? {
                Ev::Start(start) => {
                    let name = local_name(&start);
                    if name == "barrier" {
                        self.read_children(handler)?;
                    } else if !handler(self, &name, &start)? {
                        self.xml.skip_current()?;
                    }
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read(&mut self) -> Result<(), String> {
        let start = match self.xml.next()? {
            Ev::Start(e) => e,
            _ => return Err("The file is not an OpenOrienteering Mapper map.".to_string()),
        };
        if local_name(&start) != "map" {
            return Err("The file is not an OpenOrienteering Mapper map.".to_string());
        }

        self.version = attr_int(&start, "version", 0);
        if self.version > 9 {
            self.warn("The file was written by a newer version of Mapper.");
        }

        self.read_map()?;
        self.map.resolve_references();
        Ok(())
    }

    fn read_map(&mut self) -> Result<(), String> {
        let mut handler = |this: &mut Self, name: &str, start: &BytesStart| -> Result<bool, String> {
            match name {
                "georeferencing" => {
                    let scale = attr_int(start, "scale", 0);
                    if scale > 0 {
                        this.map.scale_denominator = scale;
                    }
                    this.xml.skip_current()?;
                    Ok(true)
                }
                "colors" => {
                    this.read_colors()?;
                    Ok(true)
                }
                "symbols" => {
                    this.read_symbols()?;
                    Ok(true)
                }
                "parts" => {
                    this.read_parts()?;
                    Ok(true)
                }
                _ => Ok(false),
            }
        };
        self.read_children(&mut handler)
    }

    fn read_colors(&mut self) -> Result<(), String> {
        let mut handler = |this: &mut Self, name: &str, start: &BytesStart| -> Result<bool, String> {
            if name != "color" {
                return Ok(false);
            }

            let priority = attr_int(start, "priority", -1);
            let mut color = Color {
                name: attr_str(start, "name").unwrap_or_default(),
                opacity: attr_double(start, "opacity", 1.0),
                ..Color::default()
            };

            let c = attr_double(start, "c", 0.0) as f32;
            let m = attr_double(start, "m", 0.0) as f32;
            let y = attr_double(start, "y", 0.0) as f32;
            let k = attr_double(start, "k", 0.0) as f32;
            let has_cmyk = has_attr(start, "c") || has_attr(start, "k");

            let mut rgb: Option<(f32, f32, f32)> = None;
            let mut cmyk_from_rgb = false;
            loop {
                match this.xml.next()? {
                    Ev::Start(child) => {
                        let cname = local_name(&child);
                        if cname == "rgb" {
                            let r = attr_double(&child, "r", 0.0) as f32;
                            let g = attr_double(&child, "g", 0.0) as f32;
                            let b = attr_double(&child, "b", 0.0) as f32;
                            rgb = Some((r, g, b));
                        } else if cname == "cmyk" {
                            cmyk_from_rgb = attr_str(&child, "method").as_deref() == Some("rgb");
                        }
                        this.xml.skip_current()?;
                    }
                    Ev::End | Ev::Eof => break,
                    Ev::Text(_) => {}
                }
            }

            if has_cmyk && !cmyk_from_rgb {
                color.rgb = cmyk_to_rgb(c, m, y, k);
            } else if let Some(rgb) = rgb {
                color.rgb = rgb;
            }

            if priority < 0 {
                this.map.colors.push(color);
            } else {
                let idx = priority as usize;
                if idx >= this.map.colors.len() {
                    this.map.colors.resize_with(idx + 1, Color::default);
                }
                this.map.colors[idx] = color;
            }
            Ok(true)
        };
        self.read_children(&mut handler)
    }

    fn read_symbols(&mut self) -> Result<(), String> {
        let mut handler = |this: &mut Self, name: &str, start: &BytesStart| -> Result<bool, String> {
            if name != "symbol" {
                return Ok(false);
            }
            let id = attr_int(start, "id", -1);
            if let Some(symbol) = this.read_symbol(start)? {
                this.map.symbols.push(symbol);
                this.map.symbol_ids.push(id);
            }
            Ok(true)
        };
        self.read_children(&mut handler)
    }

    fn read_symbol(&mut self, start: &BytesStart) -> Result<Option<Symbol>, String> {
        let sym_type = attr_int(start, "type", 0);
        let name = attr_str(start, "name").unwrap_or_default();
        let code = attr_str(start, "code").unwrap_or_default();
        let is_hidden = attr_bool(start, "is_hidden", false);
        let is_helper_symbol = attr_bool(start, "is_helper_symbol", false);

        let mut symbol = match sym_type {
            symbol_type::POINT => Symbol::Point(PointSymbol { name, code, is_hidden, is_helper_symbol, ..PointSymbol::new() }),
            symbol_type::LINE => Symbol::Line(LineSymbol { name, code, is_hidden, is_helper_symbol, ..LineSymbol::default() }),
            symbol_type::AREA => Symbol::Area(AreaSymbol { name, code, is_hidden, is_helper_symbol, ..AreaSymbol::new() }),
            symbol_type::TEXT => Symbol::Text(TextSymbol { name, code, is_hidden, is_helper_symbol, ..TextSymbol::default() }),
            symbol_type::COMBINED => Symbol::Combined(CombinedSymbol { name, code, is_hidden, is_helper_symbol, ..CombinedSymbol::default() }),
            _ => {
                self.warn(format!("Skipping a symbol of unknown type {}.", sym_type));
                self.xml.skip_current()?;
                return Ok(None);
            }
        };

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    let cname = local_name(&child);
                    let matches_kind = matches!(
                        (&symbol, cname.as_str()),
                        (Symbol::Point(_), "point_symbol")
                            | (Symbol::Line(_), "line_symbol")
                            | (Symbol::Area(_), "area_symbol")
                            | (Symbol::Text(_), "text_symbol")
                            | (Symbol::Combined(_), "combined_symbol")
                    );
                    if matches_kind {
                        match cname.as_str() {
                            "point_symbol" => self.read_point_symbol(&child, &mut symbol)?,
                            "line_symbol" => self.read_line_symbol(&child, &mut symbol)?,
                            "area_symbol" => self.read_area_symbol(&child, &mut symbol)?,
                            "text_symbol" => self.read_text_symbol(&child, &mut symbol)?,
                            "combined_symbol" => self.read_combined_symbol(&mut symbol)?,
                            _ => unreachable!(),
                        }
                    } else {
                        self.xml.skip_current()?;
                    }
                }
                Ev::End | Ev::Eof => break,
                Ev::Text(_) => {}
            }
        }
        Ok(Some(symbol))
    }

    fn read_point_symbol(&mut self, start: &BytesStart, symbol: &mut Symbol) -> Result<(), String> {
        point_mut(symbol).is_rotatable = attr_bool(start, "rotatable", false);
        let inner_radius = attr_length(start, "inner_radius", 0.0);
        let inner_color = attr_int(start, "inner_color", -1);
        let outer_width = attr_length(start, "outer_width", 0.0);
        let outer_color = attr_int(start, "outer_color", -1);
        {
            let p = point_mut(symbol);
            p.inner_radius = inner_radius;
            p.inner_color = inner_color;
            p.outer_width = outer_width;
            p.outer_color = outer_color;
        }

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    if local_name(&child) != "element" {
                        self.xml.skip_current()?;
                        continue;
                    }
                    let mut elem_symbol: Option<Symbol> = None;
                    let mut elem_object: Option<Object> = None;
                    loop {
                        match self.xml.next()? {
                            Ev::Start(sub) => {
                                let sname = local_name(&sub);
                                if sname == "symbol" && elem_symbol.is_none() {
                                    elem_symbol = self.read_symbol(&sub)?;
                                } else if sname == "object" && elem_symbol.is_some() {
                                    elem_object = self.read_object(&sub)?;
                                } else {
                                    self.xml.skip_current()?;
                                }
                            }
                            Ev::End | Ev::Eof => break,
                            Ev::Text(_) => {}
                        }
                    }
                    if let (Some(esym), Some(eobj)) = (elem_symbol, elem_object) {
                        point_mut(symbol).elements.push(Element {
                            symbol: esym,
                            object: eobj,
                        });
                    }
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_nested_point_symbol(&mut self) -> Result<Option<Box<PointSymbol>>, String> {
        let mut result = None;
        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    if local_name(&child) == "symbol" && result.is_none() {
                        if let Some(sym) = self.read_symbol(&child)? {
                            if let Symbol::Point(p) = sym {
                                result = Some(Box::new(p));
                            }
                        }
                    } else {
                        self.xml.skip_current()?;
                    }
                }
                Ev::End | Ev::Eof => return Ok(result),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_line_symbol(&mut self, start: &BytesStart, symbol: &mut Symbol) -> Result<(), String> {
        {
            let l = line_mut(symbol);
            l.color = attr_int(start, "color", -1);
            l.line_width = attr_length(start, "line_width", 0.0);
            l.minimum_length = attr_length(start, "minimum_length", 0.0);
            l.cap_style = attr_int(start, "cap_style", 0);
            l.join_style = attr_int(start, "join_style", 0);

            if has_attr(start, "start_offset") || has_attr(start, "end_offset") {
                l.start_offset = attr_length(start, "start_offset", 0.0);
                l.end_offset = attr_length(start, "end_offset", 0.0);
            } else if l.cap_style == cap_style::POINTED {
                l.start_offset = attr_length(start, "pointed_cap_length", 0.0);
                l.end_offset = l.start_offset;
            }

            l.dashed = attr_bool(start, "dashed", false);
            l.segment_length = attr_length(start, "segment_length", 0.0);
            l.end_length = attr_length(start, "end_length", 0.0);
            l.show_at_least_one_symbol = attr_bool(start, "show_at_least_one_symbol", false);
            l.dash_length = attr_length(start, "dash_length", 0.0);
            l.break_length = attr_length(start, "break_length", 0.0);
            l.dashes_in_group = attr_int(start, "dashes_in_group", 1).max(1);
            l.in_group_break_length = attr_length(start, "in_group_break_length", 0.0);
            l.half_outer_dashes = attr_bool(start, "half_outer_dashes", false);
            l.mid_symbols_per_spot = attr_int(start, "mid_symbols_per_spot", 1);
            l.mid_symbol_distance = attr_length(start, "mid_symbol_distance", 0.0);
            l.mid_symbol_placement = attr_int(start, "mid_symbol_placement", 0);
            l.suppress_dash_symbol_at_ends = attr_bool(start, "suppress_dash_symbol_at_ends", false);
            l.scale_dash_symbol = attr_str(start, "scale_dash_symbol").as_deref() != Some("false");
        }

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    let cname = local_name(&child);
                    match cname.as_str() {
                        "start_symbol" => {
                            let s = self.read_nested_point_symbol()?;
                            line_mut(symbol).start_symbol = s;
                        }
                        "mid_symbol" => {
                            let s = self.read_nested_point_symbol()?;
                            line_mut(symbol).mid_symbol = s;
                        }
                        "end_symbol" => {
                            let s = self.read_nested_point_symbol()?;
                            line_mut(symbol).end_symbol = s;
                        }
                        "dash_symbol" => {
                            let s = self.read_nested_point_symbol()?;
                            line_mut(symbol).dash_symbol = s;
                        }
                        "borders" => {
                            let mut count = 0;
                            loop {
                                match self.xml.next()? {
                                    Ev::Start(border_el) => {
                                        if local_name(&border_el) == "border" && count < 2 {
                                            let mut border = Border::default();
                                            self.read_border(&border_el, &mut border)?;
                                            if count == 0 {
                                                line_mut(symbol).border = border;
                                            } else {
                                                line_mut(symbol).right_border = border;
                                            }
                                        } else {
                                            self.xml.skip_current()?;
                                        }
                                        count += 1;
                                    }
                                    Ev::End | Ev::Eof => break,
                                    Ev::Text(_) => {}
                                }
                            }
                            if count == 1 {
                                let b = line_mut(symbol).border.clone();
                                line_mut(symbol).right_border = b;
                            }
                        }
                        _ => self.xml.skip_current()?,
                    }
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_border(&mut self, start: &BytesStart, border: &mut Border) -> Result<(), String> {
        border.color = attr_int(start, "color", -1);
        border.width = attr_length(start, "width", 0.0);
        border.shift = attr_length(start, "shift", 0.0);
        border.dashed = attr_bool(start, "dashed", false);
        if border.dashed {
            border.dash_length = attr_length(start, "dash_length", 2.0);
            border.break_length = attr_length(start, "break_length", 1.0);
        }
        self.xml.skip_current()
    }

    fn read_area_symbol(&mut self, start: &BytesStart, symbol: &mut Symbol) -> Result<(), String> {
        let a = area_mut(symbol);
        a.is_rotatable = attr_bool(start, "rotatable", false);
        a.color = attr_int(start, "inner_color", -1);
        a.minimum_area = attr_int(start, "min_area", 0);

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    if local_name(&child) != "pattern" {
                        self.xml.skip_current()?;
                        continue;
                    }
                    let mut pattern = FillPattern::default();
                    self.read_fill_pattern(&child, &mut pattern)?;
                    area_mut(symbol).patterns.push(pattern);
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_fill_pattern(&mut self, start: &BytesStart, pattern: &mut FillPattern) -> Result<(), String> {
        pattern.pattern_type = attr_int(start, "type", fill_pattern_type::LINE);
        pattern.no_clipping = attr_int(start, "no_clipping", 0) & 3;
        // Mapper reads the pattern angle as a float, and the rounding shifts
        // the pattern lines by a fraction of a pixel.
        pattern.angle = attr_double(start, "angle", 0.0) as f32 as f64;
        pattern.rotatable = attr_bool(start, "rotatable", false);
        pattern.line_spacing = attr_length(start, "line_spacing", 0.0);
        pattern.line_offset = attr_length(start, "line_offset", 0.0);
        pattern.offset_along_line = attr_length(start, "offset_along_line", 0.0);
        pattern.line_color = attr_int(start, "color", -1);
        pattern.line_width = attr_length(start, "line_width", 0.0);
        pattern.point_distance = attr_length(start, "point_distance", 0.0);

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    if local_name(&child) == "symbol" && pattern.point.is_none() {
                        if let Some(sym) = self.read_symbol(&child)? {
                            if let Symbol::Point(p) = sym {
                                pattern.point = Some(Box::new(p));
                            }
                        }
                    } else {
                        self.xml.skip_current()?;
                    }
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_text_symbol(&mut self, start: &BytesStart, symbol: &mut Symbol) -> Result<(), String> {
        text_mut(symbol).is_rotatable = if has_attr(start, "rotatable") {
            attr_bool(start, "rotatable", false)
        } else {
            self.version < 9
        };

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    let cname = local_name(&child);
                    match cname.as_str() {
                        "font" => {
                            let t = text_mut(symbol);
                            t.font_family = attr_str(&child, "family").unwrap_or_default();
                            t.font_size = attr_length(&child, "size", 4.0);
                            t.bold = attr_bool(&child, "bold", false);
                            t.italic = attr_bool(&child, "italic", false);
                            t.underline = attr_bool(&child, "underline", false);
                        }
                        "text" => {
                            let t = text_mut(symbol);
                            t.color = attr_int(&child, "color", -1);
                            t.line_spacing = attr_double(&child, "line_spacing", 1.0);
                            t.paragraph_spacing = attr_length(&child, "paragraph_spacing", 0.0);
                            t.character_spacing = attr_double(&child, "character_spacing", 0.0);
                            t.kerning = attr_bool(&child, "kerning", false);
                        }
                        "framing" => {
                            let t = text_mut(symbol);
                            t.framing = true;
                            t.framing_color = attr_int(&child, "color", -1);
                            t.framing_mode = attr_int(&child, "mode", 0);
                            t.framing_line_half_width = attr_length(&child, "line_half_width", 0.0);
                            t.framing_shadow_x_offset = attr_length(&child, "shadow_x_offset", 0.0);
                            t.framing_shadow_y_offset = attr_length(&child, "shadow_y_offset", 0.0);
                        }
                        "line_below" => {
                            let t = text_mut(symbol);
                            t.line_below = true;
                            t.line_below_color = attr_int(&child, "color", -1);
                            t.line_below_width = attr_length(&child, "width", 0.0);
                            t.line_below_distance = attr_length(&child, "distance", 0.0);
                        }
                        _ => {}
                    }
                    self.xml.skip_current()?;
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_combined_symbol(&mut self, symbol: &mut Symbol) -> Result<(), String> {
        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    if local_name(&child) != "part" {
                        self.xml.skip_current()?;
                        continue;
                    }
                    if attr_bool(&child, "private", false) {
                        combined_mut(symbol).part_ids.push(-1);
                        loop {
                            match self.xml.next()? {
                                Ev::Start(sub) => {
                                    if local_name(&sub) == "symbol" {
                                        if let Some(part) = self.read_symbol(&sub)? {
                                            combined_mut(symbol).owned_parts.push(part);
                                        }
                                    } else {
                                        self.xml.skip_current()?;
                                    }
                                }
                                Ev::End | Ev::Eof => break,
                                Ev::Text(_) => {}
                            }
                        }
                    } else {
                        let id = attr_int(&child, "symbol", -1);
                        combined_mut(symbol).part_ids.push(id);
                        self.xml.skip_current()?;
                    }
                }
                Ev::End | Ev::Eof => return Ok(()),
                Ev::Text(_) => {}
            }
        }
    }

    fn read_parts(&mut self) -> Result<(), String> {
        let mut handler = |this: &mut Self, name: &str, _start: &BytesStart| -> Result<bool, String> {
            if name != "part" {
                return Ok(false);
            }
            let mut inner = |this2: &mut Self, child: &str, _s: &BytesStart| -> Result<bool, String> {
                if child != "objects" {
                    return Ok(false);
                }
                this2.read_objects()?;
                Ok(true)
            };
            this.read_children(&mut inner)?;
            Ok(true)
        };
        self.read_children(&mut handler)
    }

    fn read_objects(&mut self) -> Result<(), String> {
        let mut handler = |this: &mut Self, name: &str, start: &BytesStart| -> Result<bool, String> {
            if name != "object" {
                return Ok(false);
            }
            if let Some(object) = this.read_object(start)? {
                this.map.objects.push(object);
            }
            Ok(true)
        };
        self.read_children(&mut handler)
    }

    fn read_object(&mut self, start: &BytesStart) -> Result<Option<Object>, String> {
        let obj_type = attr_int(start, "type", 0);
        let mut object = match obj_type {
            0 => Object::new(ObjectKind::Point),
            1 => Object::new(ObjectKind::Path(PathObject::default())),
            4 => Object::new(ObjectKind::Text(TextObject::default())),
            _ => {
                self.warn(format!("Skipping an object of unknown type {}.", obj_type));
                self.xml.skip_current()?;
                return Ok(None);
            }
        };

        object.symbol_id = attr_int(start, "symbol", -1);
        object.rotation = attr_double(start, "rotation", 0.0);

        if let ObjectKind::Text(t) = &mut object.kind {
            t.h_align = attr_int(start, "h_align", 0);
            t.v_align = attr_int(start, "v_align", 0);
        }

        loop {
            match self.xml.next()? {
                Ev::Start(child) => {
                    let cname = local_name(&child);
                    if cname == "coords" {
                        object.coords = self.read_coords()?;
                    } else if cname == "text" && matches!(object.kind, ObjectKind::Text(_)) {
                        let text = self.xml.read_text_content()?;
                        if let ObjectKind::Text(t) = &mut object.kind {
                            t.text = text;
                        }
                    } else if cname == "pattern" && matches!(object.kind, ObjectKind::Path(_)) {
                        let rotation = attr_double(&child, "rotation", 0.0);
                        if let ObjectKind::Path(p) = &mut object.kind {
                            p.pattern_rotation = rotation;
                        }
                        loop {
                            match self.xml.next()? {
                                Ev::Start(coord_el) => {
                                    if local_name(&coord_el) == "coord" {
                                        let x = attr_length(&coord_el, "x", 0.0);
                                        let y = attr_length(&coord_el, "y", 0.0);
                                        if let ObjectKind::Path(p) = &mut object.kind {
                                            p.pattern_origin = Point::new(x, y);
                                        }
                                    }
                                    self.xml.skip_current()?;
                                }
                                Ev::End | Ev::Eof => break,
                                Ev::Text(_) => {}
                            }
                        }
                    } else {
                        self.xml.skip_current()?;
                    }
                }
                Ev::End | Ev::Eof => break,
                Ev::Text(_) => {}
            }
        }

        // A text object may carry a second coordinate holding the size of
        // its box.
        if matches!(object.kind, ObjectKind::Text(_)) && object.coords.len() > 1 {
            object.coords.truncate(1);
        }

        Ok(Some(object))
    }

    fn read_coords(&mut self) -> Result<CoordList, String> {
        let mut coords = Vec::new();
        loop {
            match self.xml.next()? {
                Ev::Text(text) => {
                    let mut cursor: &str = &text;
                    while let Some(coord) = parse_coord(&mut cursor) {
                        coords.push(coord);
                    }
                }
                Ev::Start(start) => {
                    if local_name(&start) == "coord" {
                        let x = attr_length(&start, "x", 0.0);
                        let y = attr_length(&start, "y", 0.0);
                        let flags = attr_int(&start, "flags", 0);
                        coords.push(Coord::new(x, y, flags));
                    }
                    self.xml.skip_current()?;
                }
                Ev::End | Ev::Eof => return Ok(coords),
            }
        }
    }
}

/// Reads a map file, returning the parsed map and any recoverable warnings,
/// or an error message if the file could not be read at all.
pub fn read_xml_map(path: &Path) -> Result<(Map, Vec<String>), String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut reader = XmlMapReader::new(&content);
    reader.read()?;
    Ok((reader.map, reader.warnings))
}

/// One element of a source file, exactly as that file spells it.
#[derive(Debug, Clone)]
pub struct Fragment {
    /// The value of the element's `id` attribute, -1 where it has none.
    pub id: i32,
    /// The element and everything in it, from `<` to the last `>`.
    pub text: String,
}

/// The colours and symbols of a map file, as the file's own bytes.
///
/// [`Map`] holds what the renderer needs of a symbol, which is not all a
/// symbol is: a description, an icon, the settings of a personality the
/// renderer ignores. A tool which writes a *new map file* out of an existing
/// symbol has to keep all of it, and the way to be sure it does is to not
/// take the symbol apart at all — to copy the source's own bytes for it.
/// That also means such a tool cannot lose in translation what the reader
/// above never had, which is the difference between a symbol which renders
/// the same and a symbol which is the same.
///
/// The lists are in document order, so `symbols[i]` belongs to
/// [`Map::symbols`]`[i]` for a file whose symbols the reader all understood;
/// `id` is what an object or a combined symbol's part refers to a symbol by,
/// and is the reliable way to pair the two up.
#[derive(Debug, Default)]
pub struct Fragments {
    pub colors: Vec<Fragment>,
    pub symbols: Vec<Fragment>,
}

impl Fragments {
    /// The symbol with this `id`, as the file spells it.
    pub fn symbol(&self, id: i32) -> Option<&Fragment> {
        self.symbols.iter().find(|fragment| fragment.id == id)
    }
}

/// Reads the colour and symbol elements of a map file verbatim.
///
/// A second pass over the file, deliberately: it works on the raw event
/// stream rather than on the normalized one the reader above uses, because
/// what it is after is byte offsets, and the normalization exists precisely
/// to hide where one element ends and the next begins.
pub fn read_fragments(path: &Path) -> Result<Fragments, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut reader = Reader::from_str(&content);
    let mut fragments = Fragments::default();

    // The name of every element still open, so that a `<color>` inside
    // `<colors>` can be told from one somewhere else, and the depth a capture
    // began at can be recognised when it closes.
    let mut open: Vec<String> = Vec::new();
    // Where the element being captured began, how deep it is, what its id
    // says, and where it goes when it closes.
    let mut capture: Option<(usize, usize, i32, bool)> = None;

    loop {
        let begins = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| format!("{}: {e}", path.display()))?;
        let (start, closes) = match &event {
            Event::Start(start) => (Some(start.clone()), false),
            // A self-closing element opens and closes in one event.
            Event::Empty(start) => (Some(start.clone()), true),
            Event::End(_) => (None, true),
            Event::Eof => break,
            _ => continue,
        };

        if let Some(start) = start {
            let name = local_name(&start);
            let inside = open.last().map(String::as_str);
            if capture.is_none() {
                let wanted = match (name.as_str(), inside) {
                    ("color", Some("colors")) => Some(false),
                    ("symbol", Some("symbols")) => Some(true),
                    _ => None,
                };
                if let Some(is_symbol) = wanted {
                    capture = Some((begins, open.len(), attr_int(&start, "id", -1), is_symbol));
                }
            }
            open.push(name);
        }

        if closes {
            let depth = open.len().saturating_sub(1);
            open.pop();
            if let Some((from, at, id, is_symbol)) = capture {
                if at == depth {
                    let text = content[from..reader.buffer_position() as usize].to_string();
                    let list = if is_symbol { &mut fragments.symbols } else { &mut fragments.colors };
                    list.push(Fragment { id, text });
                    capture = None;
                }
            }
        }
    }

    Ok(fragments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_empty_map() {
        let (map, warnings) = read_xml_map(Path::new("tests/data/empty.xmap")).unwrap();
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);
        assert_eq!(map.objects.len(), 0);
    }

    #[test]
    fn reads_shapes_map() {
        let (map, warnings) = read_xml_map(Path::new("tests/data/shapes.xmap")).unwrap();
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);

        assert_eq!(map.scale_denominator, 10000);
        assert_eq!(map.colors.len(), 3);
        assert_eq!(map.colors[0].name, "Black");
        assert_eq!(map.colors[1].name, "Blue");
        // c=1,m=0.2,y=0,k=0 -> computed from CMYK (not the file's stale <rgb>
        // child), matching the real map_to_image's rendered pixel (0,204,255).
        let (r, g, b) = map.colors[1].rgb;
        assert!((r - 0.0).abs() < 1e-4, "r={r}");
        assert!((g - 0.8).abs() < 1e-4, "g={g}");
        assert!((b - 1.0).abs() < 1e-4, "b={b}");

        assert_eq!(map.symbols.len(), 5);
        assert_eq!(map.objects.len(), 5);

        // object 0: area (Open land), a 4x4 m square closed with a close-point flag
        let obj0 = &map.objects[0];
        assert!(matches!(obj0.kind, ObjectKind::Path(_)));
        assert_eq!(obj0.coords.len(), 5);
        assert!(obj0.coords[4].is_close_point());
        assert_eq!(obj0.symbol_id, 0);
        assert_eq!(obj0.symbol_index, Some(0));
        if let Symbol::Area(area) = &map.symbols[0] {
            assert_eq!(area.color, 2);
        } else {
            panic!("expected area symbol");
        }

        // object 1: area with a line fill pattern (Marsh)
        if let Symbol::Area(area) = &map.symbols[1] {
            assert_eq!(area.color, -1);
            assert_eq!(area.patterns.len(), 1);
            assert_eq!(area.patterns[0].pattern_type, fill_pattern_type::LINE);
            assert_eq!(area.patterns[0].line_color, 1);
            // line_width="150" native units -> 0.15 mm
            assert!((area.patterns[0].line_width - 0.15).abs() < 1e-9);
        } else {
            panic!("expected area symbol");
        }

        // object 2: dashed line (Path), 3 coords
        let obj2 = &map.objects[2];
        assert_eq!(obj2.coords.len(), 3);
        if let Symbol::Line(line) = &map.symbols[2] {
            assert!(line.dashed);
            assert!((line.dash_length - 2.0).abs() < 1e-9); // 2000 native units
            assert!((line.break_length - 0.5).abs() < 1e-9); // 500 native units
        } else {
            panic!("expected line symbol");
        }

        // object 3: point (Small knoll)
        let obj3 = &map.objects[3];
        assert!(matches!(obj3.kind, ObjectKind::Point));
        assert_eq!(obj3.coords.len(), 1);
        if let Symbol::Point(point) = &map.symbols[3] {
            assert!((point.inner_radius - 0.35).abs() < 1e-9); // 350 native units
            assert_eq!(point.inner_color, 0);
        } else {
            panic!("expected point symbol");
        }

        // object 4: text "Map"
        let obj4 = &map.objects[4];
        if let ObjectKind::Text(t) = &obj4.kind {
            assert_eq!(t.text, "Map");
            assert_eq!(t.h_align, h_align::HCENTER);
            assert_eq!(t.v_align, v_align::VCENTER);
        } else {
            panic!("expected text object");
        }
        if let Symbol::Text(text_symbol) = &map.symbols[4] {
            assert_eq!(text_symbol.font_family, "sans-serif");
            assert!((text_symbol.font_size - 4.0).abs() < 1e-9); // 4000 native units
            assert!(text_symbol.kerning);
        } else {
            panic!("expected text symbol");
        }
    }

    #[test]
    fn coord_parsing_handles_close_point_flags() {
        let mut cursor = "0 0;4000 0;4000 4000;0 4000;0 0 18;";
        let mut coords = Vec::new();
        while let Some(c) = parse_coord(&mut cursor) {
            coords.push(c);
        }
        assert_eq!(coords.len(), 5);
        assert_eq!(coords[4].flags, 18); // HolePoint(16) | ClosePoint(2)
        assert!(coords[4].is_close_point());
        assert!(coords[4].is_hole_point());
    }
}
