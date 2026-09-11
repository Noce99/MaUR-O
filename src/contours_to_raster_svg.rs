//! The `--create_svg` validation dumps `Contours-to-Raster.md`'s
//! "## Visualization" section asks for: one SVG after Step 0, one after Step
//! 1, and -- Step 2 being two separate passes -- one after each of its Rain
//! Drop and Anti Rain Drop Productions, so a human can check the algorithm's
//! intermediate state by opening a picture rather than parsing numbers.
//! Splitting Step 2 into its own two files, rather than overlaying both
//! colors on one, is what keeps a drop's path legible where the two passes
//! cross. Ground meters throughout, unscaled -- [`geo_svg`](https://docs.rs/geo-svg)
//! turns this crate's own `geo` geometry (already ground-meter
//! `LineString`/`Polygon`/`Coord`) straight into `<path>`/`<circle>`
//! elements and works out a `viewBox` to fit them all.
//!
//! One color per layer:
//!
//! | Layer | Meaning | Color |
//! | --- | --- | --- |
//! | grid | the raster's pixel grid | gray |
//! | pixel | a non-zero Contour Raster pixel | brown |
//! | contour (raw) | a pre-linearization (still-curved) contour | red |
//! | contour (linearized) | a post-Appendix-1 contour | green |
//! | line definer polygon | a Jump's own buffered polygon (its `LineGravityDefiners::poly`) | light blue fill |
//! | line definer arrow | one arrow at a Jump's own definition point | blue |
//! | line definer span arrows | arrows spanning a Jump's buffered polygon | yellow |
//! | heavy object polygon | a Heavy Object's own buffered polygon (`Step0Result::heavy_object_polygons`) | light pink fill |
//! | point definer arrows | one arrow at a Slope Line/Heavy Object reading's own point | red |
//! | unresolved slope line arrows | the same, for a Slope Line that found no contour to read | orange |
//! | slope line search circles | a Slope Line's own `slope_lines_contours_search_radius` ring | red if resolved, orange if not |
//! | contour gravity arrows | gravity direction along a gravity-defined contour | yellow |
//! | rain drop points | one recorded rain drop step | blue |
//! | anti rain drop points | one recorded anti rain drop step | red |
//! | drop trails | a drop's full path, source to evaporation, thin | gray |
//! | hysteresis markers | a drop point still inside its `rain_drop_starting_voting_hysteresis` window | black |
//! | vote segments | the drop step (previous position to current position) on which it cast a vote | purple |
//!
//! After Step 0: grid, pixels, every Jump's own buffered polygon (light
//! blue) and every Heavy Object's own (light pink), both contour layers,
//! both line definer arrow layers, the point
//! definer arrows, and every Slope Line's own search
//! circle -- drawn for every Slope Line found, whether or not it resolved,
//! so a skipped one's own search area can be judged by eye, colored red if
//! it resolved (matching its own arrow, already drawn by the point definer
//! arrows layer) or orange if not (which also gets its own orange arrow here,
//! since an unresolved one has no entry in `point_definers` to draw from
//! otherwise). After Step 1: the same, plus a gravity arrow along every
//! contour already resolved. After Step 2 (covering every contour, per the
//! doc's "it
//! should be impossible to have contours with undefined gravity"): the same
//! as Step 1, plus -- in one file -- every rain drop's path, and -- in a
//! second file -- every anti rain drop's path. In both: a thin gray line
//! traces each drop's whole trail first, so the path itself reads as a line
//! rather than a scatter of dots; over that, a slightly larger black circle
//! sits under any point still inside that drop's hysteresis window; over
//! that, a purple segment is drawn for the step (not a single point) on
//! which it actually cast a vote; the drop's own dot shows on top of all
//! three.
//!
//! A fifth, "final" file is written once every contour's gravity is settled
//! (after Step 2, or after Step 1 if that already resolved everything): just
//! the algorithm's actual answer -- pixels, both contour layers, and a
//! gravity arrow per node -- without the raster grid, the Jump-only definer
//! arrows, or any of Step 2's own rain-drop-path diagnostics, since those are
//! per-step working detail rather than the final picture.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use geo::{Coord, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};
use geo_svg::{Color, Style, Svg, ToSvg, ToSvgStr, ViewBox};

use crate::contour_geometry::RawVertex;
use crate::contour_raster::ContourRaster;
use crate::gravity_model::{contour_gravity_side, lwg_gravity_side, node_direction};
use crate::step0_extract::{ConflictDiagnostics, Step0Result};
use crate::step2_rain_drop::Step2Result;

const GRAY: Color = Color::Rgb(160, 160, 160);
const BROWN: Color = Color::Rgb(139, 69, 19);
const RED: Color = Color::Rgb(220, 20, 60);
const GREEN: Color = Color::Rgb(34, 139, 34);
const BLUE: Color = Color::Rgb(30, 60, 200);
const YELLOW: Color = Color::Rgb(230, 200, 20);
const BLACK: Color = Color::Rgb(0, 0, 0);
const PURPLE: Color = Color::Rgb(148, 0, 211);
const ORANGE: Color = Color::Rgb(255, 140, 0);
const LIGHT_BLUE: Color = Color::Rgb(173, 216, 230);
const LIGHT_PINK: Color = Color::Rgb(255, 182, 193);
/// [`write_conflict_svg`]'s own color for every other contour drawn only as
/// background context, well lighter than [`GRAY`] so it stays clearly
/// secondary to the two contours actually involved in the conflict.
const LIGHT_GRAY: Color = Color::Rgb(220, 220, 220);

/// How long, in ground meters, an arrow primitive is drawn.
const ARROW_LENGTH: f64 = 3.0;
/// The raw (still-curved) contour layer's stroke width, in ground meters.
const RAW_CONTOUR_STROKE_WIDTH: f32 = 0.3;
/// Half of [`RAW_CONTOUR_STROKE_WIDTH`] -- thin enough, with the raw layer
/// drawn on top of it (see [`base_layers`]), that both remain visible
/// wherever linearization tracks the curve closely.
const LINEARIZED_CONTOUR_STROKE_WIDTH: f32 = RAW_CONTOUR_STROKE_WIDTH / 2.0;
/// A rain/anti-rain drop dot's radius, in ground meters.
const DOT_RADIUS: f32 = 0.3;
/// A hysteresis marker's radius: slightly larger than [`DOT_RADIUS`] so it
/// peeks out from underneath a drop's own dot rather than being hidden by it.
const HYSTERESIS_DOT_RADIUS: f32 = DOT_RADIUS + 0.15;
/// A vote segment's stroke width, in ground meters.
const VOTE_SEGMENT_STROKE_WIDTH: f32 = 0.3;
/// A Slope Line's search-radius ring's stroke width, in ground meters: thin,
/// since it is reference context for `slope_lines_contours_search_radius`
/// rather than a reading itself.
const SEARCH_CIRCLE_STROKE_WIDTH: f32 = 0.15;
/// A Contour Raster conflict's own highlight ring's stroke width: thicker
/// than [`SEARCH_CIRCLE_STROKE_WIDTH`], since -- unlike that one, which is
/// reference context -- this ring is the whole point of the picture it is
/// drawn on ([`write_conflict_svg`]).
const CONFLICT_RING_STROKE_WIDTH: f32 = 0.5;
/// The two involved contours' own stroke width in [`write_conflict_svg`]:
/// thicker than [`LINEARIZED_CONTOUR_STROKE_WIDTH`], so they stand out from
/// every other, merely-contextual contour also drawn there.
const CONFLICT_CONTOUR_STROKE_WIDTH: f32 = LINEARIZED_CONTOUR_STROKE_WIDTH * 3.0;
/// A drop's own trail's stroke width: a third of a vote segment's, since
/// it's background context for the drop's path rather than something to
/// emphasize the way an actual vote is.
const DROP_TRAIL_STROKE_WIDTH: f32 = VOTE_SEGMENT_STROKE_WIDTH / 3.0;
/// How far, in ground meters, the viewBox is padded past the drawing's own
/// bounds so nothing is clipped at the edge.
const MARGIN: f32 = 2.0;

fn grid_lines(raster: &ContourRaster) -> MultiLineString<f64> {
    let (x0, y0) = (raster.origin.x, raster.origin.y);
    let (x1, y1) = (
        x0 + raster.width as f64 * raster.px_size,
        y0 + raster.height as f64 * raster.px_size,
    );
    let mut lines = Vec::new();
    for row in 0..=raster.height {
        let y = y0 + row as f64 * raster.px_size;
        lines.push(LineString::new(vec![
            Coord { x: x0, y },
            Coord { x: x1, y },
        ]));
    }
    for col in 0..=raster.width {
        let x = x0 + col as f64 * raster.px_size;
        lines.push(LineString::new(vec![
            Coord { x, y: y0 },
            Coord { x, y: y1 },
        ]));
    }
    MultiLineString::new(lines)
}

fn pixel_polygons(raster: &ContourRaster) -> MultiPolygon<f64> {
    let mut polys = Vec::new();
    for py in 0..raster.height {
        for px in 0..raster.width {
            if raster.get(px as i64, py as i64) == 0 {
                continue;
            }
            let x0 = raster.origin.x + px as f64 * raster.px_size;
            let y0 = raster.origin.y + py as f64 * raster.px_size;
            let (x1, y1) = (x0 + raster.px_size, y0 + raster.px_size);
            polys.push(Polygon::new(
                LineString::new(vec![
                    Coord { x: x0, y: y0 },
                    Coord { x: x1, y: y0 },
                    Coord { x: x1, y: y1 },
                    Coord { x: x0, y: y1 },
                    Coord { x: x0, y: y0 },
                ]),
                vec![],
            ));
        }
    }
    MultiPolygon::new(polys)
}

/// The raw, still-curved contour layer. Unlike every other layer here, this
/// is not `geo` geometry `geo_svg` can draw for free -- geo-types has no
/// curve type -- so `raw_polylines` keeps each vertex's own
/// `is_curve_start` flag exactly so this can draw the actual Bezier curve
/// (an SVG cubic `C` command through the next three vertices) instead of
/// connecting the anchor and control points with straight lines the way
/// treating them as an ordinary `LineString` would.
struct RawContours(Vec<Vec<RawVertex>>);

impl ToSvgStr for RawContours {
    fn to_svg_str(&self, style: &Style) -> String {
        let mut out = String::new();
        for poly in &self.0 {
            if poly.len() < 2 {
                continue;
            }
            let mut d = format!("M {:?} {:?}", poly[0].coord.x, poly[0].coord.y);
            let mut i = 0;
            while i + 1 < poly.len() {
                if poly[i].is_curve_start && i + 3 < poly.len() {
                    let (p1, p2, p3) = (poly[i + 1].coord, poly[i + 2].coord, poly[i + 3].coord);
                    let _ = write!(
                        d,
                        " C {:?} {:?} {:?} {:?} {:?} {:?}",
                        p1.x, p1.y, p2.x, p2.y, p3.x, p3.y
                    );
                    i += 3;
                } else {
                    let p = poly[i + 1].coord;
                    let _ = write!(d, " L {:?} {:?}", p.x, p.y);
                    i += 1;
                }
            }
            let _ = write!(out, r#"<path d="{d}"{style}/>"#);
        }
        out
    }

    fn viewbox(&self, _style: &Style) -> ViewBox {
        self.0.iter().flatten().fold(ViewBox::default(), |vb, v| {
            vb.add(&ViewBox::new(
                v.coord.x as f32,
                v.coord.y as f32,
                v.coord.x as f32,
                v.coord.y as f32,
            ))
        })
    }
}

fn raw_contour_lines(result: &Step0Result) -> RawContours {
    RawContours(result.raw_polylines.clone())
}

fn linearized_contour_lines(result: &Step0Result) -> MultiLineString<f64> {
    MultiLineString::new(result.contours.iter().map(|c| c.lwg.ls.clone()).collect())
}

fn arrow(from: Coord<f64>, dx: f64, dy: f64) -> LineString<f64> {
    LineString::new(vec![
        from,
        Coord {
            x: from.x + dx * ARROW_LENGTH,
            y: from.y + dy * ARROW_LENGTH,
        },
    ])
}

/// Every Jump's own buffered polygon (`LineGravityDefiners::poly`, built by
/// [`crate::contour_geometry::ls_to_polygon`]) -- the actual area Step 0's
/// `raster.pixels_in_polygon` scans to find which contours a Jump
/// intersects, so seeing it drawn is what lets a `heavy_object_width`/
/// `heavy_object_growing` choice be judged by eye against the real pixels.
fn line_definer_polygons(result: &Step0Result) -> MultiPolygon<f64> {
    MultiPolygon::new(
        result
            .line_definers
            .iter()
            .map(|definer| definer.poly.clone())
            .collect(),
    )
}

/// Every Heavy Object's own buffered polygon (`Step0Result::heavy_object_polygons`,
/// built the same way a Jump's is) -- the actual area Step 0 scans for
/// intersecting contours, so seeing it drawn is what lets a
/// `heavy_object_width`/`heavy_object_growing` choice be judged by eye
/// against the real pixels, the same as [`line_definer_polygons`] does for
/// Jumps.
fn heavy_object_polygons(result: &Step0Result) -> MultiPolygon<f64> {
    MultiPolygon::new(result.heavy_object_polygons.clone())
}

/// One arrow at a Jump's own definition point (`ls[0]`), in the direction
/// perpendicular to the Jump's line *there* -- via [`node_direction`], same
/// as [`contour_gravity_arrows`] draws a contour's, since a Jump's gravity
/// (like a contour's) is only ever meaningful relative to its own local
/// tangent, not a single vector valid along its whole length.
fn line_definer_arrows(result: &Step0Result) -> MultiLineString<f64> {
    let mut lines = Vec::new();
    for definer in &result.line_definers {
        let Some(side) = lwg_gravity_side(&definer.lwg) else {
            continue;
        };
        let ls = &definer.lwg.ls;
        let Some((dx, dy)) = node_direction(ls, 0, side) else {
            continue;
        };
        lines.push(arrow(ls.0[0], dx, dy));
    }
    MultiLineString::new(lines)
}

/// One arrow per node of a Jump's line, each perpendicular to the line *at
/// that node* (via [`node_direction`]) rather than all repeating the same
/// fixed direction -- a curved Jump's true downhill direction varies along
/// its length exactly like a contour's does (see [`line_definer_arrows`]).
fn line_definer_span_arrows(result: &Step0Result) -> MultiLineString<f64> {
    let mut lines = Vec::new();
    for definer in &result.line_definers {
        let Some(side) = lwg_gravity_side(&definer.lwg) else {
            continue;
        };
        let ls = &definer.lwg.ls;
        let n = ls.0.len();
        if n < 2 {
            continue;
        }
        let node_count = if ls.is_closed() { n - 1 } else { n };
        for i in 0..node_count {
            let Some((dx, dy)) = node_direction(ls, i, side) else {
                continue;
            };
            lines.push(arrow(ls.0[i], dx, dy));
        }
    }
    MultiLineString::new(lines)
}

/// One arrow per `PointGravityDefiners` reading (a Slope Line or a Heavy
/// Object/contour intersection's circle-fit) at its own `(x, y)`, skipping
/// any reading whose gravity could not be derived -- same shape as
/// [`line_definer_arrows`], but for a single point instead of a `LineString`.
fn point_definer_arrows(result: &Step0Result) -> MultiLineString<f64> {
    let mut lines = Vec::new();
    for definer in &result.point_definers {
        let (Some(dx), Some(dy)) = (definer.gravity_dx, definer.gravity_dy) else {
            continue;
        };
        lines.push(arrow(
            Coord {
                x: definer.x,
                y: definer.y,
            },
            dx,
            dy,
        ));
    }
    MultiLineString::new(lines)
}

/// Every Slope Line's own search-radius ring position, centered on its
/// position, at `result.slope_lines_contours_search_radius` -- drawn for
/// every Slope Line found (`result.slope_lines`) that matches `resolved`
/// (whether or not it landed on a contour, per `SlopeLineMark::resolved`),
/// so a skipped one's own search area can still be inspected to judge
/// whether the radius should be larger. Split by `resolved` rather than
/// drawn all at once so the two cases can be colored apart -- a resolved
/// Slope Line's own reading is already trustworthy (red, matching
/// [`point_definer_arrows`]), an unresolved one's is not (orange).
fn slope_line_circle_points(result: &Step0Result, resolved: bool) -> MultiPoint<f64> {
    MultiPoint::new(
        result
            .slope_lines
            .iter()
            .filter(|mark| mark.resolved == resolved)
            .map(|mark| Point::from(mark.pos))
            .collect(),
    )
}

/// An arrow (same shape as [`point_definer_arrows`]'s, which already draws
/// one for every *resolved* Slope Line/Heavy Object reading) for every Slope
/// Line that did *not* resolve into a `PointGravityDefiners` reading,
/// computed directly from its own rotation since there is no resolved
/// reading in `result.point_definers` to draw from otherwise -- so an
/// unresolved Slope Line's own intended direction is still visible, in
/// orange rather than [`point_definer_arrows`]'s red to mark it as
/// unconfirmed.
fn unresolved_slope_line_arrows(result: &Step0Result) -> MultiLineString<f64> {
    MultiLineString::new(
        result
            .slope_lines
            .iter()
            .filter(|mark| !mark.resolved)
            .map(|mark| arrow(mark.pos, -mark.rotation.sin(), -mark.rotation.cos()))
            .collect(),
    )
}

/// Gravity varies along a curved contour (it is only ever stored relative to
/// `ls[0] -> ls[1]`), so this draws one arrow per node using
/// [`node_direction`], not by repeating the stored `(gravity_dx,
/// gravity_dy)` everywhere. A closed contour repeats its first point as its
/// last (see how contours are built), so only its `len - 1` distinct nodes
/// get an arrow -- the duplicate would otherwise draw the same one twice.
fn contour_gravity_arrows(result: &Step0Result) -> MultiLineString<f64> {
    contour_gravity_arrows_filtered(result, None)
}

/// Same as [`contour_gravity_arrows`], but when `only` is given, skips any
/// contour whose index isn't `true` in it -- used by
/// [`write_step2_rain_svg`] to draw an arrow only for a contour Rain Drop
/// Production (or an earlier step) itself resolved. `result.contours` is
/// shared, mutated-in-place state: by the time any `--create_svg` file is
/// written it already holds gravity from every step that has run so far,
/// including Anti Rain Drop Production if it ran, so without this filter
/// the rain SVG would draw an arrow for a contour no rain drop ever
/// touched.
fn contour_gravity_arrows_filtered(
    result: &Step0Result,
    only: Option<&[bool]>,
) -> MultiLineString<f64> {
    let mut lines = Vec::new();
    for (idx, contour) in result.contours.iter().enumerate() {
        if let Some(mask) = only {
            if !mask.get(idx).copied().unwrap_or(false) {
                continue;
            }
        }
        let Some(side) = contour_gravity_side(contour) else {
            continue;
        };
        let ls = &contour.lwg.ls;
        let n = ls.0.len();
        if n < 2 {
            continue;
        }
        let node_count = if ls.is_closed() { n - 1 } else { n };
        for i in 0..node_count {
            let Some((dx, dy)) = node_direction(ls, i, side) else {
                continue; // a single-point degenerate contour
            };
            lines.push(arrow(ls.0[i], dx, dy));
        }
    }
    MultiLineString::new(lines)
}

fn drop_points(paths: &[Vec<Coord<f64>>]) -> MultiPoint<f64> {
    MultiPoint::new(paths.iter().flatten().map(|&c| Point::from(c)).collect())
}

/// Every drop's own full trail (source to evaporation), connecting each
/// consecutive pair of steps -- not just its vote ones -- so the drop's
/// actual path reads as a line instead of a scatter of unconnected dots.
fn drop_trails(paths: &[Vec<Coord<f64>>]) -> MultiLineString<f64> {
    MultiLineString::new(
        paths
            .iter()
            .filter(|path| path.len() >= 2)
            .map(|path| LineString::new(path.clone()))
            .collect(),
    )
}

/// A flat coordinate list -- `Step2Result`'s own `rain_hysteresis_points`/
/// `anti_rain_hysteresis_points`, already computed by
/// `step2_rain_drop::simulate_one_drop` itself, since only it knows which
/// points fell inside a hysteresis window -- turned into drawable points.
fn points(coords: &[Coord<f64>]) -> MultiPoint<f64> {
    MultiPoint::new(coords.iter().map(|&c| Point::from(c)).collect())
}

/// `Step2Result`'s own `rain_vote_segments`/`anti_rain_vote_segments` --
/// each the (previous position, current position) step a drop actually cast
/// a vote on, since a vote belongs to the step it happened on, not to
/// either endpoint alone -- turned into drawable segments.
fn segments(segments: &[(Coord<f64>, Coord<f64>)]) -> MultiLineString<f64> {
    MultiLineString::new(
        segments
            .iter()
            .map(|&(a, b)| LineString::new(vec![a, b]))
            .collect(),
    )
}

/// A line-only layer (whether `geo`'s own `MultiLineString` or our own
/// [`RawContours`]) has no area of its own; without this, a `<path>` with no
/// `fill` set still gets SVG's default opaque black fill (even for an open
/// path -- it is filled as though closed), which would paint a solid black
/// wedge under every contour and arrow.
fn line_layer<T: ToSvgStr>(geom: &T, color: Color, stroke_width: f32) -> Svg<'_> {
    geom.to_svg()
        .with_stroke_color(color)
        .with_stroke_width(stroke_width)
        .with_fill_opacity(0.0)
}

/// The Slope Line search-radius circle/unresolved-arrow layers, bundled
/// together so [`base_layers`] and [`resolved_layers`] don't need several
/// more separate positional parameters on top of everything else they
/// already take. A resolved Slope Line's own arrow is already drawn by
/// [`point_definer_arrows`] (red, alongside Heavy Object readings) -- this
/// only adds `unresolved_arrows` (orange, see
/// [`unresolved_slope_line_arrows`]) plus both circle groups.
struct SlopeLineLayers<'a> {
    resolved_circles: &'a MultiPoint<f64>,
    unresolved_circles: &'a MultiPoint<f64>,
    circle_radius: f32,
    unresolved_arrows: &'a MultiLineString<f64>,
}

/// An unfilled search-radius ring per point, in `color`.
fn circle_layer(points: &MultiPoint<f64>, radius: f32, color: Color) -> Svg<'_> {
    points
        .to_svg()
        .with_radius(radius)
        .with_fill_opacity(0.0)
        .with_stroke_color(color)
        .with_stroke_width(SEARCH_CIRCLE_STROKE_WIDTH)
}

/// Draws the raw (red) layer before the linearized (green) one -- later
/// layers paint over earlier ones in SVG, so this keeps the linearized line
/// on top wherever the two coincide, with the wider red line still peeking
/// out on either side.
#[allow(clippy::too_many_arguments)]
fn base_layers<'a>(
    grid: &'a MultiLineString<f64>,
    pixels: &'a MultiPolygon<f64>,
    line_polygons: &'a MultiPolygon<f64>,
    heavy_object_polygons: &'a MultiPolygon<f64>,
    raw: &'a RawContours,
    linearized: &'a MultiLineString<f64>,
    line_arrows: &'a MultiLineString<f64>,
    line_span_arrows: &'a MultiLineString<f64>,
    point_arrows: &'a MultiLineString<f64>,
    slope_lines: SlopeLineLayers<'a>,
) -> Svg<'a> {
    pixels
        .to_svg()
        .with_fill_color(BROWN)
        .with_fill_opacity(0.5)
        .with_stroke_opacity(0.0)
        .and(
            line_polygons
                .to_svg()
                .with_fill_color(LIGHT_BLUE)
                .with_fill_opacity(0.4)
                .with_stroke_opacity(0.0),
        )
        .and(
            heavy_object_polygons
                .to_svg()
                .with_fill_color(LIGHT_PINK)
                .with_fill_opacity(0.4)
                .with_stroke_opacity(0.0),
        )
        .and(line_layer(grid, GRAY, 0.05))
        .and(line_layer(raw, RED, RAW_CONTOUR_STROKE_WIDTH))
        .and(line_layer(
            linearized,
            GREEN,
            LINEARIZED_CONTOUR_STROKE_WIDTH,
        ))
        .and(line_layer(line_arrows, BLUE, 0.2))
        .and(line_layer(line_span_arrows, YELLOW, 0.15))
        .and(line_layer(point_arrows, RED, 0.2))
        .and(line_layer(slope_lines.unresolved_arrows, ORANGE, 0.2))
        .and(circle_layer(
            slope_lines.resolved_circles,
            slope_lines.circle_radius,
            RED,
        ))
        .and(circle_layer(
            slope_lines.unresolved_circles,
            slope_lines.circle_radius,
            ORANGE,
        ))
}

/// Pads the composed drawing's own bounds by [`MARGIN`] and renders it, so
/// nothing sits flush against the SVG's edge.
fn finish(mut svg: Svg) -> String {
    svg.viewbox = svg.viewbox().with_margin(MARGIN);
    svg.to_string()
}

fn write(path: &Path, svg: Svg) -> Result<(), String> {
    fs::write(path, finish(svg)).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Writes the SVG asked for after Step 0: the raster grid and its non-zero
/// pixels, every contour raw and linearized, the Jump ("LineGravityDefiners")
/// arrows, and the Slope Line/Heavy Object ("PointGravityDefiners") arrows.
pub fn write_step0_svg(path: &Path, result: &Step0Result) -> Result<(), String> {
    let grid = grid_lines(&result.raster);
    let pixels = pixel_polygons(&result.raster);
    let raw = raw_contour_lines(result);
    let linearized = linearized_contour_lines(result);
    let line_polygons = line_definer_polygons(result);
    let heavy_object_polygons = heavy_object_polygons(result);
    let line_arrows = line_definer_arrows(result);
    let line_span_arrows = line_definer_span_arrows(result);
    let point_arrows = point_definer_arrows(result);
    let resolved_circles = slope_line_circle_points(result, true);
    let unresolved_circles = slope_line_circle_points(result, false);
    let unresolved_arrows = unresolved_slope_line_arrows(result);
    write(
        path,
        base_layers(
            &grid,
            &pixels,
            &line_polygons,
            &heavy_object_polygons,
            &raw,
            &linearized,
            &line_arrows,
            &line_span_arrows,
            &point_arrows,
            SlopeLineLayers {
                resolved_circles: &resolved_circles,
                unresolved_circles: &unresolved_circles,
                circle_radius: result.slope_lines_contours_search_radius as f32,
                unresolved_arrows: &unresolved_arrows,
            },
        ),
    )
}

/// [`base_layers`] plus a gravity arrow along every contour already
/// resolved -- what Step 1's own SVG shows, and what each of Step 2's two
/// files (rain, anti rain) build further on.
#[allow(clippy::too_many_arguments)]
fn resolved_layers<'a>(
    grid: &'a MultiLineString<f64>,
    pixels: &'a MultiPolygon<f64>,
    line_polygons: &'a MultiPolygon<f64>,
    heavy_object_polygons: &'a MultiPolygon<f64>,
    raw: &'a RawContours,
    linearized: &'a MultiLineString<f64>,
    line_arrows: &'a MultiLineString<f64>,
    line_span_arrows: &'a MultiLineString<f64>,
    point_arrows: &'a MultiLineString<f64>,
    slope_lines: SlopeLineLayers<'a>,
    gravity_arrows: &'a MultiLineString<f64>,
) -> Svg<'a> {
    base_layers(
        grid,
        pixels,
        line_polygons,
        heavy_object_polygons,
        raw,
        linearized,
        line_arrows,
        line_span_arrows,
        point_arrows,
        slope_lines,
    )
    .and(line_layer(gravity_arrows, YELLOW, 0.2))
}

/// The same as [`write_step0_svg`], plus a gravity arrow along every contour
/// Step 1 (or Step 0's direct evidence) has already resolved.
pub fn write_step1_svg(path: &Path, result: &Step0Result) -> Result<(), String> {
    let grid = grid_lines(&result.raster);
    let pixels = pixel_polygons(&result.raster);
    let raw = raw_contour_lines(result);
    let linearized = linearized_contour_lines(result);
    let line_polygons = line_definer_polygons(result);
    let heavy_object_polygons = heavy_object_polygons(result);
    let line_arrows = line_definer_arrows(result);
    let line_span_arrows = line_definer_span_arrows(result);
    let point_arrows = point_definer_arrows(result);
    let resolved_circles = slope_line_circle_points(result, true);
    let unresolved_circles = slope_line_circle_points(result, false);
    let unresolved_arrows = unresolved_slope_line_arrows(result);
    let gravity_arrows = contour_gravity_arrows(result);
    write(
        path,
        resolved_layers(
            &grid,
            &pixels,
            &line_polygons,
            &heavy_object_polygons,
            &raw,
            &linearized,
            &line_arrows,
            &line_span_arrows,
            &point_arrows,
            SlopeLineLayers {
                resolved_circles: &resolved_circles,
                unresolved_circles: &unresolved_circles,
                circle_radius: result.slope_lines_contours_search_radius as f32,
                unresolved_arrows: &unresolved_arrows,
            },
            &gravity_arrows,
        ),
    )
}

/// The algorithm's actual answer, once every contour's gravity is settled:
/// pixels, both contour layers (raw red under linearized green), and one
/// gravity arrow per node (yellow) -- no raster grid, no Jump-only definer
/// arrows (also yellow -- leaving them out keeps the gravity arrows the only
/// thing that color), and none of Step 2's own rain-drop-path layers, since
/// those are per-step working detail rather than the final picture. Call
/// this only after every contour is resolved (Step 1 alone, or Step 1 and
/// Step 2 together) -- an earlier call would just draw whatever gravity
/// happens to be set so far, silently mislabeled as final.
pub fn write_final_svg(path: &Path, result: &Step0Result) -> Result<(), String> {
    let pixels = pixel_polygons(&result.raster);
    let raw = raw_contour_lines(result);
    let linearized = linearized_contour_lines(result);
    let gravity_arrows = contour_gravity_arrows(result);
    write(
        path,
        pixels
            .to_svg()
            .with_fill_color(BROWN)
            .with_fill_opacity(0.5)
            .with_stroke_opacity(0.0)
            .and(line_layer(&raw, RED, RAW_CONTOUR_STROKE_WIDTH))
            .and(line_layer(
                &linearized,
                GREEN,
                LINEARIZED_CONTOUR_STROKE_WIDTH,
            ))
            .and(line_layer(&gravity_arrows, YELLOW, 0.2)),
    )
}

/// The same as [`write_step1_svg`], plus every Rain Drop Production drop's
/// full trail (gray, thin -- background context so the drop's actual path
/// reads as a line rather than a scatter of dots), its own path (blue dots,
/// over a black marker under any point `step2.rain_hysteresis_points` names
/// as still inside a hysteresis window, and a purple segment over that for
/// each (previous position, current position) step `step2.rain_vote_segments`
/// names as where it actually cast a vote -- a vote is a property of the
/// step it happened on, not of either endpoint, so it is drawn as the step
/// itself rather than a dot that would otherwise just sit on top of the
/// hysteresis marker) -- kept to its own file, apart from
/// [`write_step2_anti_rain_svg`]'s, so a drop's path stays legible where the
/// two passes cross rather than overlaying both colors on one picture. Its
/// gravity-arrow layer is filtered to `step2.defined_after_rain`, so it
/// never shows an arrow for a
/// contour only Anti Rain Drop Production went on to resolve, even though
/// `result.contours` itself already holds that final state by the time this
/// runs.
pub fn write_step2_rain_svg(
    path: &Path,
    result: &Step0Result,
    step2: &Step2Result,
) -> Result<(), String> {
    let grid = grid_lines(&result.raster);
    let pixels = pixel_polygons(&result.raster);
    let raw = raw_contour_lines(result);
    let linearized = linearized_contour_lines(result);
    let line_polygons = line_definer_polygons(result);
    let heavy_object_polygons = heavy_object_polygons(result);
    let line_arrows = line_definer_arrows(result);
    let line_span_arrows = line_definer_span_arrows(result);
    let point_arrows = point_definer_arrows(result);
    let resolved_circles = slope_line_circle_points(result, true);
    let unresolved_circles = slope_line_circle_points(result, false);
    let unresolved_arrows = unresolved_slope_line_arrows(result);
    let gravity_arrows = contour_gravity_arrows_filtered(result, Some(&step2.defined_after_rain));
    let trails = drop_trails(&step2.rain_paths);
    let hysteresis_marks = points(&step2.rain_hysteresis_points);
    let vote_lines = segments(&step2.rain_vote_segments);
    let rain = drop_points(&step2.rain_paths);
    write(
        path,
        resolved_layers(
            &grid,
            &pixels,
            &line_polygons,
            &heavy_object_polygons,
            &raw,
            &linearized,
            &line_arrows,
            &line_span_arrows,
            &point_arrows,
            SlopeLineLayers {
                resolved_circles: &resolved_circles,
                unresolved_circles: &unresolved_circles,
                circle_radius: result.slope_lines_contours_search_radius as f32,
                unresolved_arrows: &unresolved_arrows,
            },
            &gravity_arrows,
        )
        .and(line_layer(&trails, GRAY, DROP_TRAIL_STROKE_WIDTH))
        .and(
            hysteresis_marks
                .to_svg()
                .with_radius(HYSTERESIS_DOT_RADIUS)
                .with_fill_color(BLACK)
                .with_stroke_opacity(0.0),
        )
        .and(line_layer(&vote_lines, PURPLE, VOTE_SEGMENT_STROKE_WIDTH))
        .and(
            rain.to_svg()
                .with_radius(DOT_RADIUS)
                .with_fill_color(BLUE)
                .with_stroke_opacity(0.0),
        ),
    )
}

/// The same as [`write_step1_svg`], plus every Anti Rain Drop Production
/// drop's full trail, path, hysteresis marker and vote segment, the same
/// way as [`write_step2_rain_svg`] -- see there for why this is a separate
/// file rather than a second layer on the same one.
pub fn write_step2_anti_rain_svg(
    path: &Path,
    result: &Step0Result,
    step2: &Step2Result,
) -> Result<(), String> {
    let grid = grid_lines(&result.raster);
    let pixels = pixel_polygons(&result.raster);
    let raw = raw_contour_lines(result);
    let linearized = linearized_contour_lines(result);
    let line_polygons = line_definer_polygons(result);
    let heavy_object_polygons = heavy_object_polygons(result);
    let line_arrows = line_definer_arrows(result);
    let line_span_arrows = line_definer_span_arrows(result);
    let point_arrows = point_definer_arrows(result);
    let resolved_circles = slope_line_circle_points(result, true);
    let unresolved_circles = slope_line_circle_points(result, false);
    let unresolved_arrows = unresolved_slope_line_arrows(result);
    let gravity_arrows = contour_gravity_arrows(result);
    let trails = drop_trails(&step2.anti_rain_paths);
    let hysteresis_marks = points(&step2.anti_rain_hysteresis_points);
    let vote_lines = segments(&step2.anti_rain_vote_segments);
    let anti_rain = drop_points(&step2.anti_rain_paths);
    write(
        path,
        resolved_layers(
            &grid,
            &pixels,
            &line_polygons,
            &heavy_object_polygons,
            &raw,
            &linearized,
            &line_arrows,
            &line_span_arrows,
            &point_arrows,
            SlopeLineLayers {
                resolved_circles: &resolved_circles,
                unresolved_circles: &unresolved_circles,
                circle_radius: result.slope_lines_contours_search_radius as f32,
                unresolved_arrows: &unresolved_arrows,
            },
            &gravity_arrows,
        )
        .and(line_layer(&trails, GRAY, DROP_TRAIL_STROKE_WIDTH))
        .and(
            hysteresis_marks
                .to_svg()
                .with_radius(HYSTERESIS_DOT_RADIUS)
                .with_fill_color(BLACK)
                .with_stroke_opacity(0.0),
        )
        .and(line_layer(&vote_lines, PURPLE, VOTE_SEGMENT_STROKE_WIDTH))
        .and(
            anti_rain
                .to_svg()
                .with_radius(DOT_RADIUS)
                .with_fill_color(RED)
                .with_stroke_opacity(0.0),
        ),
    )
}

/// The contiguous run of `ls`'s own points that stays within `radius` ground
/// meters of `center`, containing whichever point is nearest to it.
///
/// A contour involved in a Contour Raster conflict can be arbitrarily long
/// -- most of a map's own extent, in the real case this was written for --
/// while the conflict itself only ever happens at one small, local spot on
/// it. `geo_svg` always auto-fits its output `viewBox` to the full bounds of
/// everything it is asked to draw (see [`finish`]), with no way to draw a
/// shape while excluding it from that fit; drawing either contour's own full
/// length in [`write_conflict_svg`] would zoom the picture out to the whole
/// map instead of the small area that actually matters. Clipping to a local
/// window first, rather than drawing the whole line and cropping the view
/// after the fact, is what keeps the picture actually zoomed in.
fn local_window(ls: &LineString<f64>, center: Coord<f64>, radius: f64) -> LineString<f64> {
    let pts = &ls.0;
    if pts.is_empty() {
        return LineString::new(Vec::new());
    }
    let dist = |p: &Coord<f64>| (p.x - center.x).hypot(p.y - center.y);
    let nearest = pts
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| dist(a).total_cmp(&dist(b)))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut start = nearest;
    while start > 0 && dist(&pts[start - 1]) <= radius {
        start -= 1;
    }
    let mut end = nearest;
    while end + 1 < pts.len() && dist(&pts[end + 1]) <= radius {
        end += 1;
    }
    LineString::new(pts[start..=end].to_vec())
}

/// Every other already-accepted contour's own local window around `center`
/// (see [`local_window`]) -- background context for
/// [`write_conflict_svg`]'s picture, so the two contours actually involved
/// in the conflict can be judged against the shape of the terrain around
/// them rather than floating alone. Skips `existing_contour_idx` (drawn
/// separately, highlighted) and any contour with nothing inside the window
/// (most of a map's own contours, for a small local conflict).
fn other_local_contour_lines(
    diagnostics: &ConflictDiagnostics,
    center: Coord<f64>,
    radius: f64,
) -> MultiLineString<f64> {
    MultiLineString::new(
        diagnostics
            .contours_so_far
            .iter()
            .enumerate()
            .filter(|&(idx, _)| idx as u64 != diagnostics.existing_contour_idx)
            .map(|(_, contour)| local_window(&contour.lwg.ls, center, radius))
            .filter(|ls| ls.0.len() >= 2)
            .collect(),
    )
}

/// One filled square, `px_size` wide, centered on each of `positions` -- the
/// exact pixels a conflict happened on, drawn solid so they are legible even
/// where the two contours' own lines sit almost on top of each other. Takes
/// a plain slice, not `ConflictDiagnostics` itself, so [`write_conflict_svg`]
/// can pass only the ones inside its own local window rather than every
/// conflicting pixel there is -- for a long run of conflicts (a genuinely
/// too-closely-spaced pair, not a digitizing-gap splice) that can be nearly
/// all of a contour's own length, and every one of them left in would drag
/// `geo_svg`'s auto-fitted `viewBox` right back out to it (see
/// `local_window`'s own doc comment).
fn conflict_pixel_polygons(positions: &[Coord<f64>], px_size: f64) -> MultiPolygon<f64> {
    let half = px_size / 2.0;
    MultiPolygon::new(
        positions
            .iter()
            .map(|&center| {
                Polygon::new(
                    LineString::new(vec![
                        Coord {
                            x: center.x - half,
                            y: center.y - half,
                        },
                        Coord {
                            x: center.x + half,
                            y: center.y - half,
                        },
                        Coord {
                            x: center.x + half,
                            y: center.y + half,
                        },
                        Coord {
                            x: center.x - half,
                            y: center.y + half,
                        },
                        Coord {
                            x: center.x - half,
                            y: center.y - half,
                        },
                    ]),
                    vec![],
                )
            })
            .collect(),
    )
}

/// The centroid of every conflicting pixel, and a radius that reaches a few
/// pixels past the furthest one from it, capped at [`CONFLICT_RING_MAX_RADIUS`]
/// -- so [`write_conflict_svg`]'s own ring stays a single circle drawn
/// clearly around the conflicting cluster (never collapsing to a dot for a
/// single-pixel conflict, and never growing past a sane size for a long run
/// of them -- a genuinely too-closely-spaced pair, not a digitizing-gap
/// splice, can otherwise span most of a contour's own length) rather than
/// one ring per pixel, which would be an unreadable pile of overlapping
/// circles.
fn conflict_ring(diagnostics: &ConflictDiagnostics) -> (MultiPoint<f64>, f32) {
    let positions = &diagnostics.conflict_positions;
    let n = positions.len() as f64;
    let centroid = positions
        .iter()
        .fold(Coord { x: 0.0, y: 0.0 }, |acc, p| Coord {
            x: acc.x + p.x / n,
            y: acc.y + p.y / n,
        });
    let furthest = positions
        .iter()
        .map(|p| (p.x - centroid.x).hypot(p.y - centroid.y))
        .fold(0.0_f64, f64::max);
    let radius = (furthest + 3.0 * diagnostics.raster.px_size).min(CONFLICT_RING_MAX_RADIUS) as f32;
    (MultiPoint::new(vec![Point::from(centroid)]), radius)
}

/// The most [`conflict_ring`]'s own drawn radius ever grows to, in ground
/// meters, regardless of how far its own conflicting pixels actually spread
/// -- `Point`'s own `viewbox` (in the `geo_svg` crate this module is built
/// on) grows to include a drawn circle's full radius, so an uncapped ring
/// around a long run of conflicts would defeat [`local_window`]'s whole
/// point the same way drawing that whole run's own contour length would.
const CONFLICT_RING_MAX_RADIUS: f64 = 30.0;
/// How far past [`conflict_ring`]'s own (already-capped) radius
/// [`write_conflict_svg`] clips each contour, filters which conflicting
/// pixels it draws, and draws its local pixel grid, in multiples of that
/// radius -- generous enough to still show real local shape/curvature around
/// the conflict, not just the bare colliding segment.
const LOCAL_WINDOW_FACTOR: f64 = 6.0;
/// A floor under [`LOCAL_WINDOW_FACTOR`]'s own window, in ground meters, so
/// a single-pixel conflict (where [`conflict_ring`]'s radius is already tiny)
/// still gets a picture with real spatial context rather than a close-up of
/// nothing.
const LOCAL_WINDOW_MIN: f64 = 15.0;

/// The most conflicting pixels [`write_conflict_svg`] ever draws as their
/// own solid squares -- a genuinely too-closely-spaced pair of contours (not
/// a small digitizing-gap splice) can conflict on thousands of pixels in a
/// row, and past a few hundred, the individual squares stop adding real
/// information over just knowing the cluster is large (which the ring
/// already conveys) while still adding to the file linearly forever.
const MAX_DRAWN_CONFLICT_PIXELS: usize = 300;

/// Writes a diagnostic picture of a Contour Raster conflict `step0_extract::extract`
/// could not resolve by unifying the two contours involved (see
/// `ExtractError::Conflict`): every other already-accepted contour in light
/// gray, for terrain context, the already-accepted contour actually involved
/// (red) and the newly read one that never made it in (orange) -- each
/// clipped to a window around the conflict (see `local_window`, since any of
/// them can otherwise be most of the map) -- every conflicting pixel inside
/// that same window as its own solid red square, and one red ring around the
/// whole conflicting cluster.
pub fn write_conflict_svg(path: &Path, diagnostics: &ConflictDiagnostics) -> Result<(), String> {
    let (ring_center, ring_radius) = conflict_ring(diagnostics);
    let Some(&centroid) = ring_center.0.first() else {
        return Err("a conflict with no conflicting pixels was reported".to_string());
    };
    let centroid = centroid.0;
    let window_radius = (ring_radius as f64 * LOCAL_WINDOW_FACTOR).max(LOCAL_WINDOW_MIN);

    // Only the conflicting pixels inside the local window itself, and even
    // then capped -- see `conflict_pixel_polygons`'s and
    // `MAX_DRAWN_CONFLICT_PIXELS`'s own doc comments for why a long run of
    // them cannot all be drawn here the way `write_step0_svg` draws every
    // one of `result.raster`'s own non-zero pixels.
    let local_positions: Vec<Coord<f64>> = diagnostics
        .conflict_positions
        .iter()
        .copied()
        .filter(|p| (p.x - centroid.x).hypot(p.y - centroid.y) <= window_radius)
        .take(MAX_DRAWN_CONFLICT_PIXELS)
        .collect();
    let conflict_pixels = conflict_pixel_polygons(&local_positions, diagnostics.raster.px_size);

    let other_contours = other_local_contour_lines(diagnostics, centroid, window_radius);
    let existing_contour = MultiLineString::new(vec![local_window(
        &diagnostics.contours_so_far[diagnostics.existing_contour_idx as usize]
            .lwg
            .ls,
        centroid,
        window_radius,
    )]);
    let new_contour = MultiLineString::new(vec![local_window(
        &diagnostics.new_ls,
        centroid,
        window_radius,
    )]);

    write(
        path,
        line_layer(&other_contours, LIGHT_GRAY, LINEARIZED_CONTOUR_STROKE_WIDTH)
            .and(line_layer(
                &existing_contour,
                RED,
                CONFLICT_CONTOUR_STROKE_WIDTH,
            ))
            .and(line_layer(
                &new_contour,
                ORANGE,
                CONFLICT_CONTOUR_STROKE_WIDTH,
            ))
            .and(
                conflict_pixels
                    .to_svg()
                    .with_fill_color(RED)
                    .with_fill_opacity(0.7)
                    .with_stroke_opacity(0.0),
            )
            .and(
                ring_center
                    .to_svg()
                    .with_radius(ring_radius)
                    .with_fill_opacity(0.0)
                    .with_stroke_color(RED)
                    .with_stroke_width(CONFLICT_RING_STROKE_WIDTH),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour_raster::ContourRaster;
    use crate::gravity_model::{
        gravity_vector_for_side, Contour, LineGravityDefiners, LineWithGravity,
        PointGravityDefiners,
    };
    use crate::step0_extract::SlopeLineMark;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn v(x: f64, y: f64, is_curve_start: bool) -> RawVertex {
        RawVertex {
            coord: c(x, y),
            is_curve_start,
        }
    }

    fn sample_result() -> Step0Result {
        let ls = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0), c(10.0, 0.0)]);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ls.clone()),
            elevation_height: None,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(1.0);

        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 12, 3);
        raster.write_contour(0, &ls).unwrap();

        Step0Result {
            contours: vec![contour],
            raw_polylines: vec![vec![
                v(0.0, 0.0, false),
                v(5.0, 0.5, false),
                v(10.0, 0.0, false),
            ]],
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    fn viewbox_width(svg_text: &str) -> f32 {
        let viewbox = svg_text
            .split("viewBox=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("viewBox attribute missing");
        viewbox.split_whitespace().nth(2).unwrap().parse().unwrap()
    }

    #[test]
    fn conflict_svg_highlights_both_contours_and_rings_the_conflicting_pixels() {
        let existing = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0)])),
            elevation_height: None,
        };
        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 25, 25);
        raster.write_contour(0, &existing.lwg.ls).unwrap();

        let diagnostics = ConflictDiagnostics {
            raster,
            contours_so_far: vec![existing],
            new_ls: LineString::new(vec![c(0.2, 0.0), c(5.2, 0.0)]),
            existing_contour_idx: 0,
            conflict_positions: vec![c(0.5, 0.5), c(1.5, 0.5)],
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.svg");
        write_conflict_svg(&path, &diagnostics).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(!text.is_empty());
        // The involved contour (red) and the newly read one (orange) both
        // drawn, plus one red ring around the conflicting cluster.
        assert!(text.contains(r#"stroke="rgb(220,20,60)""#));
        assert!(text.contains(r#"stroke="rgb(255,140,0)""#));
        assert_eq!(
            count(&text, "<circle"),
            1,
            "exactly one ring, not one per pixel"
        );
    }

    #[test]
    fn conflict_svg_draws_nearby_other_contours_light_gray_but_leaves_out_far_away_ones() {
        let existing = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0)])),
            elevation_height: None,
        };
        // Close enough to fall inside the local window: real context.
        let nearby = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 3.0), c(5.0, 3.0)])),
            elevation_height: None,
        };
        // A thousand meters away: must be left out entirely, or it would
        // drag the auto-fitted viewBox back out to the whole map.
        let far_away = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 1000.0), c(5.0, 1000.0)])),
            elevation_height: None,
        };
        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 25, 1010);
        raster.write_contour(0, &existing.lwg.ls).unwrap();
        raster.write_contour(1, &nearby.lwg.ls).unwrap();
        raster.write_contour(2, &far_away.lwg.ls).unwrap();

        let diagnostics = ConflictDiagnostics {
            raster,
            contours_so_far: vec![existing, nearby, far_away],
            new_ls: LineString::new(vec![c(0.2, 0.0), c(5.2, 0.0)]),
            existing_contour_idx: 0,
            conflict_positions: vec![c(0.5, 0.5), c(1.5, 0.5)],
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.svg");
        write_conflict_svg(&path, &diagnostics).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(
            text.contains(r#"stroke="rgb(220,220,220)""#),
            "the nearby other contour should be drawn in light gray"
        );
        assert!(
            viewbox_width(&text) < 200.0,
            "the far-away other contour must not be drawn -- it would blow up the viewBox"
        );
    }

    #[test]
    fn conflict_svg_stays_zoomed_in_even_when_either_contour_spans_most_of_the_map() {
        // Both contours run for a kilometer; they only ever collide in one
        // small spot, at the very start of each. Drawing either one's whole
        // length would drag geo_svg's auto-fitted viewBox out to the whole
        // map instead of the small area that actually matters.
        let long_existing = LineString::new(vec![c(0.0, 0.0), c(1000.0, 0.0)]);
        let long_new = LineString::new(vec![c(0.2, 0.0), c(1000.2, 0.0)]);
        let existing = Contour {
            lwg: LineWithGravity::new(long_existing.clone()),
            elevation_height: None,
        };
        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 1005, 5);
        raster.write_contour(0, &long_existing).unwrap();

        let diagnostics = ConflictDiagnostics {
            raster,
            contours_so_far: vec![existing],
            new_ls: long_new,
            existing_contour_idx: 0,
            conflict_positions: vec![c(0.5, 0.5)],
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.svg");
        write_conflict_svg(&path, &diagnostics).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(
            viewbox_width(&text) < 200.0,
            "expected a local, zoomed-in viewBox around the conflict, not one stretched to \
             cover a kilometer-long contour"
        );
    }

    #[test]
    fn conflict_svg_stays_zoomed_in_even_over_a_long_run_of_conflicting_pixels() {
        // Not just a long contour (the previous test) but a long *run of
        // conflicting pixels itself* -- the genuine "too closely spaced,
        // must still crash" case, not a small digitizing-gap splice. Every
        // one of those pixels, and the ring around them, must still stay
        // capped to a sane local size rather than ballooning the viewBox out
        // to cover the whole run.
        let long_existing = LineString::new(vec![c(0.0, 0.0), c(1000.0, 0.0)]);
        let long_new = LineString::new(vec![c(0.0, 0.3), c(1000.0, 0.3)]);
        let existing = Contour {
            lwg: LineWithGravity::new(long_existing.clone()),
            elevation_height: None,
        };
        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 1005, 5);
        raster.write_contour(0, &long_existing).unwrap();
        let conflict_positions: Vec<Coord<f64>> =
            (0..1000).map(|x| c(x as f64 + 0.5, 0.5)).collect();

        let diagnostics = ConflictDiagnostics {
            raster,
            contours_so_far: vec![existing],
            new_ls: long_new,
            existing_contour_idx: 0,
            conflict_positions,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.svg");
        write_conflict_svg(&path, &diagnostics).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(
            viewbox_width(&text) < 500.0,
            "a thousand-pixel-long conflict must still be capped to a bounded, zoomed-in \
             picture, not one stretched across the whole run"
        );
        // Only the conflicting pixels inside the local window (roughly
        // 2*window_radius wide) are drawn, not all thousand of them.
        assert!(count(&text, "<path") < 500, "{}", count(&text, "<path"));
    }

    #[test]
    fn grid_lines_cover_every_row_and_column() {
        let raster = ContourRaster::new(c(0.0, 0.0), 1.0, 4, 3);
        assert_eq!(grid_lines(&raster).0.len(), (4 + 1) + (3 + 1));
    }

    #[test]
    fn pixel_polygons_match_non_zero_pixels() {
        let result = sample_result();
        // The 10m-long horizontal contour at y=0 through a raster whose
        // origin is (-1, -1): every pixel it touches, one polygon each.
        assert!(!pixel_polygons(&result.raster).0.is_empty());
    }

    #[test]
    fn raw_contour_draws_a_true_bezier_curve_not_straight_control_segments() {
        let raw = RawContours(vec![vec![
            v(0.0, 0.0, true),
            v(1.0, 1.0, false),
            v(2.0, 1.0, false),
            v(3.0, 0.0, false),
        ]]);
        let svg_str = raw.to_svg_str(&Style::default());
        assert!(
            svg_str.contains(r#"d="M 0.0 0.0 C 1.0 1.0 2.0 1.0 3.0 0.0""#),
            "expected a single cubic Bezier command through the curve's own control \
             points, not straight segments between them; got: {svg_str}"
        );
    }

    #[test]
    fn red_raw_contour_is_drawn_under_green_linearized_contour_at_half_the_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0.svg");
        let result = sample_result();
        write_step0_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        let red_pos = text.find("rgb(220,20,60)").expect("red contour missing");
        let green_pos = text.find("rgb(34,139,34)").expect("green contour missing");
        assert!(
            red_pos < green_pos,
            "the red (raw) layer must be drawn before the green (linearized) layer, \
             so the linearized line stays on top wherever the two coincide"
        );

        assert_eq!(
            LINEARIZED_CONTOUR_STROKE_WIDTH,
            RAW_CONTOUR_STROKE_WIDTH / 2.0
        );
        assert!(text.contains(&format!(r#"stroke-width="{RAW_CONTOUR_STROKE_WIDTH}""#)));
        assert!(text.contains(&format!(
            r#"stroke-width="{LINEARIZED_CONTOUR_STROKE_WIDTH}""#
        )));
    }

    #[test]
    fn step0_svg_has_contours_and_no_gravity_arrows_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0.svg");
        let result = sample_result();
        write_step0_svg(&path, &result).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("<svg"));
        assert!(text.trim_end().ends_with("</svg>"));
        assert_eq!(count(&text, "rgb(220,20,60)"), 1); // one raw contour segment
        assert_eq!(count(&text, "rgb(34,139,34)"), 1); // one linearized contour segment
        assert_eq!(count(&text, "rgb(230,200,20)"), 0); // no gravity arrows yet
    }

    #[test]
    fn step1_svg_adds_one_gravity_arrow_per_node() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
        let result = sample_result();
        write_step1_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count(&text, "rgb(230,200,20)"), 3);
    }

    #[test]
    fn point_definer_arrows_skip_undefined_gravity_and_use_own_position() {
        let mut result = sample_result();
        result.point_definers = vec![
            PointGravityDefiners {
                x: 3.0,
                y: 7.0,
                reference_contour: 0,
                gravity_dx: Some(1.0),
                gravity_dy: Some(0.0),
            },
            // No gravity reading was derived for this one -- it draws no arrow.
            PointGravityDefiners {
                x: 5.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: None,
                gravity_dy: None,
            },
        ];
        let arrows = point_definer_arrows(&result);
        assert_eq!(
            arrows.0.len(),
            1,
            "the undefined-gravity reading draws no arrow"
        );
        let ls = &arrows.0[0];
        assert_eq!(ls.0[0], c(3.0, 7.0));
        assert_eq!(
            (ls.0[1].x - ls.0[0].x, ls.0[1].y - ls.0[0].y),
            (ARROW_LENGTH, 0.0)
        );
    }

    #[test]
    fn step0_svg_draws_a_red_point_definer_arrow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0.svg");
        let mut result = sample_result();
        result.point_definers = vec![PointGravityDefiners {
            x: 3.0,
            y: 7.0,
            reference_contour: 0,
            gravity_dx: Some(1.0),
            gravity_dy: Some(0.0),
        }];
        write_step0_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // one raw contour segment plus one point definer arrow, both red.
        assert_eq!(count(&text, "rgb(220,20,60)"), 2);
    }

    #[test]
    fn unresolved_slope_line_arrows_skip_resolved_marks() {
        let mut result = sample_result();
        // rotation = 0 -> gravity (-sin(0), -cos(0)) = (0, -1): straight down.
        result.slope_lines = vec![
            SlopeLineMark {
                pos: c(10.0, 20.0),
                rotation: 0.0,
                resolved: false,
            },
            SlopeLineMark {
                pos: c(0.0, 0.0),
                rotation: 0.0,
                resolved: true,
            },
        ];
        let arrows = unresolved_slope_line_arrows(&result);
        assert_eq!(arrows.0.len(), 1, "the resolved mark draws no arrow here");
        let ls = &arrows.0[0];
        assert_eq!(ls.0[0], c(10.0, 20.0));
        assert_eq!(
            (ls.0[1].x - ls.0[0].x, ls.0[1].y - ls.0[0].y),
            (0.0, -ARROW_LENGTH)
        );
    }

    #[test]
    fn slope_line_circle_points_splits_by_resolved() {
        let mut result = sample_result();
        result.slope_lines = vec![
            SlopeLineMark {
                pos: c(3.0, 7.0),
                rotation: 0.0,
                resolved: true,
            },
            SlopeLineMark {
                pos: c(-2.0, 1.0),
                rotation: 1.0,
                resolved: false,
            },
        ];
        let resolved = slope_line_circle_points(&result, true);
        let unresolved = slope_line_circle_points(&result, false);
        assert_eq!(resolved.0, vec![Point::from(c(3.0, 7.0))]);
        assert_eq!(unresolved.0, vec![Point::from(c(-2.0, 1.0))]);
    }

    #[test]
    fn step0_svg_colors_slope_line_circles_and_arrows_by_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0.svg");
        let mut result = sample_result();
        // A resolved reading also needs a matching point_definers entry --
        // that's what actually draws its (red) arrow, mirroring how Step 0
        // itself only ever produces a resolved SlopeLineMark alongside one.
        result.point_definers = vec![PointGravityDefiners {
            x: 3.0,
            y: 7.0,
            reference_contour: 0,
            gravity_dx: Some(0.0),
            gravity_dy: Some(-1.0),
        }];
        result.slope_lines = vec![
            SlopeLineMark {
                pos: c(3.0, 7.0),
                rotation: 0.0,
                resolved: true,
            },
            SlopeLineMark {
                pos: c(-2.0, 1.0),
                rotation: 1.0,
                resolved: false,
            },
        ];
        result.slope_lines_contours_search_radius = 4.5;
        write_step0_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert_eq!(count(&text, "<circle"), 2);
        assert_eq!(count(&text, r#"r="4.5""#), 2);
        // resolved: 1 circle + 1 arrow (from point_definers), both red,
        // alongside the raw contour's own unrelated red.
        assert_eq!(count(&text, "rgb(220,20,60)"), 3);
        // unresolved: 1 circle + 1 arrow, both orange.
        assert_eq!(count(&text, "rgb(255,140,0)"), 2);

        let circle_start = text.find("<circle").expect("search circle missing");
        let circle_end = circle_start + text[circle_start..].find("/>").unwrap();
        let circle_tag = &text[circle_start..circle_end];
        assert!(
            circle_tag.contains(r#"fill-opacity="0""#),
            "the search circle must be an unfilled ring, not a filled disc; got: {circle_tag}"
        );
    }

    #[test]
    fn step0_svg_fills_a_line_definer_polygon_light_blue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0.svg");
        let mut result = sample_result();
        let ls = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0)]);
        let mut lwg = LineWithGravity::new(ls);
        lwg.gravity_dx = Some(0.0);
        lwg.gravity_dy = Some(1.0);
        let poly = Polygon::new(
            LineString::new(vec![
                c(0.0, -1.0),
                c(5.0, -1.0),
                c(5.0, 1.0),
                c(0.0, 1.0),
                c(0.0, -1.0),
            ]),
            vec![],
        );
        result.line_definers = vec![LineGravityDefiners { lwg, poly }];
        write_step0_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        let tag_start = text
            .find(r#"fill="rgb(173,216,230)""#)
            .and_then(|pos| text[..pos].rfind('<'))
            .expect("light blue fill missing");
        let tag_end = tag_start + text[tag_start..].find("/>").unwrap();
        let tag = &text[tag_start..tag_end];
        assert!(
            tag.contains(r#"fill-opacity="0.4""#),
            "expected the light blue polygon to not be fully opaque; got: {tag}"
        );
    }

    #[test]
    fn step0_svg_fills_a_heavy_object_polygon_light_pink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0.svg");
        let mut result = sample_result();
        result.heavy_object_polygons = vec![Polygon::new(
            LineString::new(vec![
                c(0.0, -1.0),
                c(5.0, -1.0),
                c(5.0, 1.0),
                c(0.0, 1.0),
                c(0.0, -1.0),
            ]),
            vec![],
        )];
        write_step0_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        let tag_start = text
            .find(r#"fill="rgb(255,182,193)""#)
            .and_then(|pos| text[..pos].rfind('<'))
            .expect("light pink fill missing");
        let tag_end = tag_start + text[tag_start..].find("/>").unwrap();
        let tag = &text[tag_start..tag_end];
        assert!(
            tag.contains(r#"fill-opacity="0.4""#),
            "expected the light pink polygon to not be fully opaque; got: {tag}"
        );
    }

    fn result_for(ls: LineString<f64>, side: f64) -> Step0Result {
        let (gx, gy) = gravity_vector_for_side(&ls, side);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
        };
        contour.lwg.gravity_dx = Some(gx);
        contour.lwg.gravity_dy = Some(gy);
        Step0Result {
            contours: vec![contour],
            raw_polylines: vec![Vec::new()],
            raster: ContourRaster::new(c(-1.0, -1.0), 1.0, 30, 30),
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// An arrow's own direction (unnormalized, `ARROW_LENGTH`-scaled), as
    /// `contour_gravity_arrows` draws it.
    fn arrow_dir(arrows: &MultiLineString<f64>, i: usize) -> (f64, f64) {
        let ls = &arrows.0[i];
        (ls.0[1].x - ls.0[0].x, ls.0[1].y - ls.0[0].y)
    }

    #[test]
    fn gravity_arrow_at_a_bend_averages_the_incoming_and_outgoing_perpendiculars() {
        // An open "L": (0,0) -> (10,0) -> (10,10).
        let ls = LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0), c(10.0, 10.0)]);
        let result = result_for(ls, 1.0);
        let arrows = contour_gravity_arrows(&result);
        assert_eq!(arrows.0.len(), 3, "one arrow per node, no duplicates");

        // Node 0 has no preceding segment: its arrow is the following
        // segment's own perpendicular, (0,1), alone.
        let (dx0, dy0) = arrow_dir(&arrows, 0);
        assert!((dx0).abs() < 1e-9 && (dy0 - ARROW_LENGTH).abs() < 1e-9);

        // Node 2 has no following segment: its arrow is the preceding
        // segment's own perpendicular, (-1,0), alone.
        let (dx2, dy2) = arrow_dir(&arrows, 2);
        assert!((dx2 + ARROW_LENGTH).abs() < 1e-9 && dy2.abs() < 1e-9);

        // Node 1 (the bend) sits between both: the mean of (0,1) and
        // (-1,0), normalized -- diagonal, not just the outgoing segment's
        // own (-1,0) the way a single-sided reading would give.
        let (dx1, dy1) = arrow_dir(&arrows, 1);
        let expected = (-1.0 / 2.0f64.sqrt(), 1.0 / 2.0f64.sqrt());
        assert!((dx1 / ARROW_LENGTH - expected.0).abs() < 1e-9);
        assert!((dy1 / ARROW_LENGTH - expected.1).abs() < 1e-9);
    }

    #[test]
    fn gravity_arrows_on_a_closed_contour_wrap_and_are_not_duplicated() {
        // A closed square; the last point repeats the first, as every
        // closed contour in this crate's own data does.
        let ls = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 10.0),
            c(0.0, 10.0),
            c(0.0, 0.0),
        ]);
        let result = result_for(ls, 1.0);
        let arrows = contour_gravity_arrows(&result);
        // 4 distinct nodes, not 5 -- the repeated closing point must not
        // draw the same arrow twice.
        assert_eq!(arrows.0.len(), 4);

        // Node 0's "previous" segment wraps to the contour's own last real
        // segment, (0,10)->(0,0) (perpendicular (1,0)), averaged with its
        // next segment (0,0)->(10,0) (perpendicular (0,1)) -- diagonal, not
        // simply the next segment's own (0,1).
        let (dx0, dy0) = arrow_dir(&arrows, 0);
        let expected = (1.0 / 2.0f64.sqrt(), 1.0 / 2.0f64.sqrt());
        assert!((dx0 / ARROW_LENGTH - expected.0).abs() < 1e-9);
        assert!((dy0 / ARROW_LENGTH - expected.1).abs() < 1e-9);
    }

    #[test]
    fn final_svg_has_contours_and_gravity_but_no_grid_or_definer_arrows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("final.svg");
        let result = sample_result();
        write_final_svg(&path, &result).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("<svg"));
        assert!(text.trim_end().ends_with("</svg>"));
        assert_eq!(count(&text, "rgb(220,20,60)"), 1); // raw contour
        assert_eq!(count(&text, "rgb(34,139,34)"), 1); // linearized contour
        assert_eq!(count(&text, "rgb(230,200,20)"), 3); // one gravity arrow per node
        assert_eq!(count(&text, "rgb(160,160,160)"), 0); // no raster grid
        assert_eq!(count(&text, "rgb(30,60,200)"), 0); // no line definer arrows
    }

    #[test]
    fn step2_rain_svg_has_only_rain_drop_points() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2_rain.svg");
        let result = sample_result();
        let step2 = Step2Result {
            resolved_by_rain: 0,
            resolved_by_anti_rain: 0,
            ambiguous_warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: vec![vec![c(1.0, 0.0)]],
            rain_hysteresis_points: Vec::new(),
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };
        write_step2_rain_svg(&path, &result, &step2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count(&text, "<circle"), 3); // only the 3 rain-drop dots, no hysteresis markers
        assert_eq!(count(&text, "rgb(220,20,60)"), 1); // the raw contour, not an anti-rain-drop dot
    }

    #[test]
    fn step2_rain_svg_omits_a_gravity_arrow_for_a_contour_only_anti_rain_resolved() {
        // `result.contours` is shared, mutated-in-place state: by the time
        // any --create_svg file is written it already holds gravity from
        // every step that ran, including Anti Rain Drop Production. Without
        // `defined_after_rain` filtering the rain SVG's own gravity-arrow
        // layer, a contour no rain drop ever touched -- resolved only by
        // Anti Rain Drop Production, which by this point has already run --
        // would still show an arrow here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2_rain.svg");

        let ls0 = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0), c(10.0, 0.0)]);
        let ls1 = LineString::new(vec![c(0.0, 5.0), c(5.0, 5.0), c(10.0, 5.0)]);
        let (gx0, gy0) = gravity_vector_for_side(&ls0, 1.0);
        let (gx1, gy1) = gravity_vector_for_side(&ls1, 1.0);
        let mut contour0 = Contour {
            lwg: LineWithGravity::new(ls0),
            elevation_height: None,
        };
        contour0.lwg.gravity_dx = Some(gx0);
        contour0.lwg.gravity_dy = Some(gy0);
        let mut contour1 = Contour {
            lwg: LineWithGravity::new(ls1),
            elevation_height: None,
        };
        contour1.lwg.gravity_dx = Some(gx1);
        contour1.lwg.gravity_dy = Some(gy1);

        let result = Step0Result {
            contours: vec![contour0, contour1],
            raw_polylines: vec![Vec::new(), Vec::new()],
            raster: ContourRaster::new(c(-1.0, -1.0), 1.0, 30, 30),
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            warnings: Vec::new(),
        };

        let step2 = Step2Result {
            resolved_by_rain: 0,
            resolved_by_anti_rain: 0,
            ambiguous_warnings: Vec::new(),
            defined_after_rain: vec![true, false],
            rain_paths: Vec::new(),
            anti_rain_paths: Vec::new(),
            rain_hysteresis_points: Vec::new(),
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };

        write_step2_rain_svg(&path, &result, &step2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 3 nodes on each contour; only contour 0's arrows should be drawn.
        assert_eq!(count(&text, "rgb(230,200,20)"), 3);
    }

    #[test]
    fn step2_rain_svg_draws_a_gray_trail_under_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2_rain.svg");
        let result = sample_result();
        let step2 = Step2Result {
            resolved_by_rain: 0,
            resolved_by_anti_rain: 0,
            ambiguous_warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: Vec::new(),
            rain_hysteresis_points: vec![c(0.0, 0.0)],
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: vec![(c(0.0, 0.0), c(0.0, 1.0))],
            anti_rain_vote_segments: Vec::new(),
        };
        write_step2_rain_svg(&path, &result, &step2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        // One gray trail path -- one rain path, drawn as a single polyline
        // through all 3 of its points, not per-step segments.
        let trail_width_attr = format!(r#"stroke-width="{DROP_TRAIL_STROKE_WIDTH}""#);
        assert_eq!(count(&text, &trail_width_attr), 1);

        let trail_pos = text.find(&trail_width_attr).expect("drop trail missing");
        let black_pos = text.find("rgb(0,0,0)").expect("hysteresis marker missing");
        let purple_pos = text
            .find(r#"stroke="rgb(148,0,211)""#)
            .expect("vote segment missing");
        let blue_pos = text.find("rgb(30,60,200)").expect("rain-drop dot missing");
        assert!(
            trail_pos < black_pos && black_pos < purple_pos && purple_pos < blue_pos,
            "expected draw order gray trail < black (hysteresis) < purple (vote) < blue \
             (drop dot), so each layer stays visible over the more general one(s) beneath it"
        );
    }

    #[test]
    fn step2_anti_rain_svg_has_only_anti_rain_drop_points() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2_anti_rain.svg");
        let result = sample_result();
        let step2 = Step2Result {
            resolved_by_rain: 0,
            resolved_by_anti_rain: 0,
            ambiguous_warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: vec![vec![c(1.0, 0.0)]],
            rain_hysteresis_points: Vec::new(),
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };
        write_step2_anti_rain_svg(&path, &result, &step2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count(&text, "<circle"), 1); // only the 1 anti-rain-drop dot, no hysteresis markers
    }

    #[test]
    fn step2_rain_svg_draws_a_black_marker_under_each_hysteresis_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2_rain.svg");
        let result = sample_result();
        let step2 = Step2Result {
            resolved_by_rain: 0,
            resolved_by_anti_rain: 0,
            ambiguous_warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: Vec::new(),
            // Whichever points step2_rain_drop::simulate_one_drop reported
            // as still inside a hysteresis window -- this layer just draws
            // them, it doesn't recompute which ones those are.
            rain_hysteresis_points: vec![c(0.0, 0.0), c(0.0, 1.0)],
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };
        write_step2_rain_svg(&path, &result, &step2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        // 3 rain-drop dots, plus a black marker under each of the 2
        // hysteresis points.
        assert_eq!(count(&text, "<circle"), 5);
        assert_eq!(count(&text, r#"fill="rgb(0,0,0)""#), 2);

        let black_pos = text.find("rgb(0,0,0)").expect("hysteresis marker missing");
        let blue_pos = text.find("rgb(30,60,200)").expect("rain-drop dot missing");
        assert!(
            black_pos < blue_pos,
            "the hysteresis marker must be drawn before the rain-drop dot, so the \
             dot's own color still shows on top"
        );
    }

    #[test]
    fn step2_rain_svg_draws_a_purple_segment_for_each_vote_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2_rain.svg");
        let result = sample_result();
        let step2 = Step2Result {
            resolved_by_rain: 0,
            resolved_by_anti_rain: 0,
            ambiguous_warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: Vec::new(),
            rain_hysteresis_points: vec![c(0.0, 0.0)],
            anti_rain_hysteresis_points: Vec::new(),
            // Whichever step step2_rain_drop::simulate_one_drop reported as
            // where it actually cast a vote -- this layer just draws it, it
            // doesn't recompute which one that is.
            rain_vote_segments: vec![(c(0.0, 0.0), c(0.0, 1.0))],
            anti_rain_vote_segments: Vec::new(),
        };
        write_step2_rain_svg(&path, &result, &step2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        // 3 rain-drop dots and 1 black hysteresis marker are circles; the
        // vote is drawn as its own line segment, not a circle.
        assert_eq!(count(&text, "<circle"), 4);
        assert_eq!(count(&text, r#"stroke="rgb(148,0,211)""#), 1);

        let black_pos = text.find("rgb(0,0,0)").expect("hysteresis marker missing");
        let purple_pos = text
            .find(r#"stroke="rgb(148,0,211)""#)
            .expect("vote segment missing");
        let blue_pos = text.find("rgb(30,60,200)").expect("rain-drop dot missing");
        assert!(
            black_pos < purple_pos && purple_pos < blue_pos,
            "expected draw order black (hysteresis) < purple (vote segment) < blue (drop \
             dot), so a dot's own color always wins, and the vote segment still crosses \
             over whichever hysteresis marker it touches instead of only covering it"
        );
    }
}
