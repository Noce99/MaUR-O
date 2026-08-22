//! The same map, written as SVG instead of drawn as pixels.
//!
//! [`Renderer::paint_rect`](crate::renderer::Renderer::paint_rect) rasterizes
//! the renderables it has built; this writes those same renderables out as
//! vector paths, in the same order, with the same colours, widths, caps,
//! joins and clips. What comes out is what would have been drawn, at whatever
//! size it is later displayed or printed at.
//!
//! ```no_run
//! use maur_o::{renderer::Renderer, xml_reader};
//!
//! let (map, _) = xml_reader::read_xml_map_str("<map/>").unwrap();
//! let svg = Renderer::new(&map).to_svg(None, &[]);
//! std::fs::write("map.svg", svg).unwrap();
//! ```
//!
//! # Why this can be a plain list of paths
//!
//! By the time the renderer has built its renderables there are no symbols
//! left, and no text: a line's dashes are separate pieces of geometry, an
//! area's pattern is the shapes it is filled with, and a label is the
//! outlines of its glyphs. That is what makes the two outputs the same
//! picture rather than two interpretations of one — and it is why no fonts
//! are embedded here, and why nothing needs a `<text>` element.
//!
//! # Units
//!
//! Millimetres on the paper, which is what the renderables are in. The `<svg>`
//! carries its size in millimetres and a `viewBox` in the same units, so the
//! file prints at the size the map is.

use std::fmt::Write;

use crate::geometry::{PathCommand, PenCap, PenJoin, Rect};
use crate::map::Color;
use crate::renderer::{Renderer, TINY_SKIA_MITER_LIMIT};

/// How many decimal places coordinates are written to.
///
/// Four, in millimetres, is a ten-thousandth of a millimetre: finer than any
/// printing process, and finer than the format the map came from stores.
const DECIMALS: usize = 4;

/// Writes a number without trailing zeros, which on a map of a hundred
/// thousand coordinates is most of the file.
fn num(value: f64, out: &mut String) {
    let rounded = format!("{value:.DECIMALS$}");
    let trimmed = if rounded.contains('.') {
        rounded.trim_end_matches('0').trim_end_matches('.')
    } else {
        &rounded
    };
    // "-0" is a zero with a sign on it.
    out.push_str(if trimmed == "-0" { "0" } else { trimmed });
}

/// A colour as CSS, and its opacity separately -- SVG keeps the two apart.
fn css_color(color: &Color) -> String {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.rgb.0),
        channel(color.rgb.1),
        channel(color.rgb.2)
    )
}

/// Writes a path's commands as an SVG `d` attribute.
fn path_data(commands: &[PathCommand], out: &mut String) {
    let mut last_command = ' ';
    for command in commands {
        match *command {
            PathCommand::MoveTo(p) => {
                out.push('M');
                num(p.x, out);
                out.push(' ');
                num(p.y, out);
                last_command = 'M';
            }
            PathCommand::LineTo(p) => {
                // A repeated command letter may be left out, which on a long
                // polyline saves a byte a point.
                if last_command != 'L' {
                    out.push('L');
                    last_command = 'L';
                } else {
                    out.push(' ');
                }
                num(p.x, out);
                out.push(' ');
                num(p.y, out);
            }
            PathCommand::CubicTo(c1, c2, end) => {
                if last_command != 'C' {
                    out.push('C');
                    last_command = 'C';
                } else {
                    out.push(' ');
                }
                for p in [c1, c2, end] {
                    num(p.x, out);
                    out.push(' ');
                    num(p.y, out);
                    out.push(' ');
                }
                out.pop();
            }
            PathCommand::Close => {
                out.push('Z');
                last_command = 'Z';
            }
        }
    }
}

fn cap_name(cap: PenCap) -> &'static str {
    match cap {
        PenCap::Flat => "butt",
        PenCap::Round => "round",
        PenCap::Square => "square",
    }
}

fn join_name(join: PenJoin) -> &'static str {
    match join {
        PenJoin::Miter => "miter",
        PenJoin::Bevel => "bevel",
        PenJoin::Round => "round",
    }
}

impl Renderer<'_> {
    /// Writes the map as an SVG document.
    ///
    /// `clip_mm` leaves out everything outside a rectangle, and
    /// `hidden_symbol_ids` everything drawn with those symbols -- the same
    /// two filters, meaning the same things, as
    /// [`paint_rect`](Renderer::paint_rect). Passing `None` and an empty
    /// slice writes the whole map.
    ///
    /// The document is sized to what it contains, in millimetres.
    pub fn to_svg(&self, clip_mm: Option<Rect>, hidden_symbol_ids: &[i32]) -> String {
        let extent = clip_mm.unwrap_or_else(|| self.extent());
        let mut out = String::new();
        let (width, height) = (extent.width().max(0.0), extent.height().max(0.0));

        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"");
        num(width, &mut out);
        out.push_str("mm\" height=\"");
        num(height, &mut out);
        out.push_str("mm\" viewBox=\"");
        for value in [extent.left(), extent.top(), width, height] {
            num(value, &mut out);
            out.push(' ');
        }
        out.pop();
        out.push_str("\">\n");

        self.write_body(clip_mm, hidden_symbol_ids, &mut out);
        out.push_str("</svg>\n");
        out
    }

    /// The paths themselves, and the clip paths they refer to.
    fn write_body(&self, clip_mm: Option<Rect>, hidden_symbol_ids: &[i32], out: &mut String) {
        let order = self.draw_order(clip_mm, hidden_symbol_ids);

        // Only the clips actually used, named once each at the top.
        let mut used_clips: Vec<usize> = order
            .iter()
            .filter_map(|&i| self.renderables[i].clip)
            .collect();
        used_clips.sort_unstable();
        used_clips.dedup();
        if !used_clips.is_empty() {
            out.push_str("<defs>\n");
            for &clip in &used_clips {
                let _ = write!(
                    out,
                    "<clipPath id=\"c{clip}\" clipPathUnits=\"userSpaceOnUse\"><path d=\""
                );
                path_data(&self.clips[clip].commands, out);
                // An area outline punches its holes by the even-odd rule,
                // which is what it was built for and what it is filled with.
                out.push_str("\" clip-rule=\"evenodd\"/></clipPath>\n");
            }
            out.push_str("</defs>\n");
        }

        let mut open_clip: Option<usize> = None;
        for &index in &order {
            let renderable = &self.renderables[index];
            let Some(color) = self.map.color(renderable.color) else {
                continue;
            };
            let clip = renderable.clip;
            if clip != open_clip {
                if open_clip.is_some() {
                    out.push_str("</g>\n");
                }
                if let Some(clip) = clip {
                    let _ = writeln!(out, "<g clip-path=\"url(#c{clip})\">");
                }
                open_clip = clip;
            }
            self.write_renderable(index, color, out);
        }
        if open_clip.is_some() {
            out.push_str("</g>\n");
        }
    }

    fn write_renderable(&self, index: usize, color: &Color, out: &mut String) {
        let fill = css_color(color);
        out.push_str("<path d=\"");
        let renderable = &self.renderables[index];
        path_data(&renderable.path.commands, out);
        out.push('"');

        if let Some(transform) = renderable.transform {
            out.push_str(" transform=\"matrix(");
            for value in [
                f64::from(transform.sx),
                f64::from(transform.ky),
                f64::from(transform.kx),
                f64::from(transform.sy),
                f64::from(transform.tx),
                f64::from(transform.ty),
            ] {
                num(value, out);
                out.push(',');
            }
            out.pop();
            out.push_str(")\"");
        }

        if renderable.pen_width > 0.0 {
            out.push_str(" fill=\"none\" stroke=\"");
            out.push_str(&fill);
            out.push_str("\" stroke-width=\"");
            num(renderable.pen_width, out);
            let _ = write!(
                out,
                "\" stroke-linecap=\"{}\" stroke-linejoin=\"{}\"",
                cap_name(renderable.cap),
                join_name(renderable.join)
            );
            if matches!(renderable.join, PenJoin::Miter) {
                out.push_str(" stroke-miterlimit=\"");
                num(TINY_SKIA_MITER_LIMIT, out);
                out.push('"');
            }
            if color.opacity < 1.0 {
                out.push_str(" stroke-opacity=\"");
                num(color.opacity, out);
                out.push('"');
            }
        } else {
            out.push_str(" fill=\"");
            out.push_str(&fill);
            out.push('"');
            if renderable.fill_rule == tiny_skia::FillRule::EvenOdd {
                out.push_str(" fill-rule=\"evenodd\"");
            }
            if color.opacity < 1.0 {
                out.push_str(" fill-opacity=\"");
                num(color.opacity, out);
                out.push('"');
            }
        }
        out.push_str("/>\n");
    }
}
