//! The `--create_svg` validation dumps `Contours-to-Raster.md`'s
//! "## Visualization" section asks for: one SVG after Step 1 (before its own
//! Growing Process sub-step runs), one after each of that sub-step's own two
//! phases (Seeking, then Matching), one after Step 2, and -- Step 3 being two
//! separate passes -- one after each of its Rain Drop and Anti Rain Drop
//! Productions, so a human can check the algorithm's intermediate state by
//! opening a picture rather than parsing numbers. Splitting Step 3 into its
//! own two files, rather than overlaying both colors on one, is what keeps a
//! drop's path legible where the two passes cross; Seeking and Matching get
//! their own two files for a related reason -- the Seeking file shows what
//! Phase 1 managed strictly on its own, before Phase 2's own, different
//! physics (and whatever it went on to resolve or merge) gets a chance to
//! obscure it. Ground meters throughout, unscaled --
//! [`geo_svg`](https://docs.rs/geo-svg) turns this crate's own `geo`
//! geometry (already ground-meter `LineString`/`Polygon`/`Coord`) straight
//! into `<path>`/`<circle>` elements and works out a `viewBox` to fit them
//! all.
//!
//! One color per layer:
//!
//! | Layer | Meaning | Color |
//! | --- | --- | --- |
//! | out-of-bound area | the whole `OUT_OF_BOUND` region, traced into a handful of rings rather than one square per pixel (see [`oob_area`]) | black |
//! | grid | the raster's pixel grid | gray |
//! | pixel | a non-zero Contour Raster pixel | brown |
//! | contour (raw) | a pre-linearization (still-curved) contour | red |
//! | contour (linearized) | a post-Appendix-1 contour | green |
//! | line definer polygon | a Jump's own buffered polygon (its `LineGravityDefiners::poly`) | light blue fill |
//! | line definer arrow | one arrow at a Jump's own definition point | blue |
//! | line definer span arrows | arrows spanning a Jump's buffered polygon, its own gravity vector | purple |
//! | heavy object polygon | a Heavy Object's own buffered polygon (`Step1Result::heavy_object_polygons`) | light pink fill |
//! | point definer arrows | one arrow at a Slope Line/Heavy Object reading's own point | red |
//! | unresolved slope line arrows | the same, for a Slope Line that found no contour to read | orange |
//! | slope line search circles | a Slope Line's own `slope_lines_contours_search_radius` ring | red if resolved, orange if not |
//! | contour gravity arrows | gravity direction along a gravity-defined contour | yellow |
//! | vote reading circles | Step 2 only -- a ring around every position that cast a Heavy Object/Jump vote (see [`vote_reading_points`]) | dark green |
//! | rain drop points | one recorded rain drop step | blue |
//! | anti rain drop points | one recorded anti rain drop step | red |
//! | drop trails | a drop's full path, source to evaporation, thin | gray |
//! | hysteresis markers | a drop point still inside its `rain_drop_starting_voting_hysteresis` window | black |
//! | vote segments | the drop step (previous position to current position) on which it cast a vote | purple |
//!
//! After Step 1: grid, pixels, every Jump's own buffered polygon (light
//! blue) and every Heavy Object's own (light pink), both contour layers,
//! both line definer arrow layers, the point
//! definer arrows, and every Slope Line's own search
//! circle -- drawn for every Slope Line found, whether or not it resolved,
//! so a skipped one's own search area can be judged by eye, colored red if
//! it resolved (matching its own arrow, already drawn by the point definer
//! arrows layer) or orange if not (which also gets its own orange arrow here,
//! since an unresolved one has no entry in `point_definers` to draw from
//! otherwise). After each of Seeking and Matching: the same, plus any
//! contour the Growing Process has touched *so far* (in either phase) drawn
//! in blue instead of green, plus every integration step *that one phase
//! itself* took drawn as its own four push/pull vectors and a green dot at
//! its own resulting position -- Matching's own file starts that one layer
//! fresh rather than continuing Seeking's own, so each file shows only its
//! own phase's own steps. After Step 2: the same as Matching's own file, plus
//! a gravity arrow along every
//! contour already resolved, plus a dark green ring around every position
//! that contributed a Heavy Object or Jump reading to Step 2's
//! confidence-weighted vote -- whether or not that reading's own contour
//! ended up resolved by the vote, a Slope Line, or the closed-hill
//! heuristic, so the evidence behind a warning (or a vote's own win) stays
//! visible -- this one ring layer is Step 2's own file only, not repeated in
//! either Step 3 file below. After Step 3 (covering every contour, per the
//! doc's "it
//! should be impossible to have contours with undefined gravity"): the same
//! as Step 2 minus that ring layer, plus -- in one file -- every rain drop's
//! path, and -- in a
//! second file -- every anti rain drop's path. In both: a thin gray line
//! traces each drop's whole trail first, so the path itself reads as a line
//! rather than a scatter of dots; over that, a slightly larger black circle
//! sits under any point still inside that drop's hysteresis window; over
//! that, a purple segment is drawn for the step (not a single point) on
//! which it actually cast a vote; the drop's own dot shows on top of all
//! three.
//!
//! One more, unnumbered "final" file is written once every contour's gravity
//! is settled (after Step 3, or after Step 2 if that already resolved
//! everything): just the algorithm's actual answer -- pixels, both contour
//! layers, and a gravity arrow per node -- without the raster grid, the
//! Jump-only definer arrows, or any of Step 3's own rain-drop-path
//! diagnostics, since those are per-step working detail rather than the
//! final picture.
//!
//! After Step 4: `08_<map_name>_step5_gravity.svg`, Step 5's own per-pixel
//! Gravity Direction sub-step ([`crate::step5_gravity_raster`]) -- the same
//! grid/pixel/heavy-object picture Steps 1-2 draw (see [`base_layers`]),
//! plus one segment per pixel, tail at its own center, showing the per-pixel
//! downhill direction that sub-step resolved, colored red (intense) to blue
//! (weak) by its own magnitude (a pixel whose own contributing readings
//! nearly cancel out gets no segment at all -- see
//! [`GRAVITY_DIRECTION_MIN_MAGNITUDE`]).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use geo::{
    Coord, Euclidean, InterpolateLine, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};
use geo_svg::{Color, Style, Svg, ToSvg, ToSvgStr, ViewBox};

use crate::contour_geometry::RawVertex;
use crate::contour_raster::{ContourRaster, CONTOUR_0_MATRIX_VALUE, HIGH_DENSITY, OUT_OF_BOUND};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{
    contour_gravity_side, lwg_gravity_side, node_direction, Contour, GravityReadingSource,
};
use crate::step1_extract::{contour_force_magnitude, Step1Result};
use crate::step3_rain_drop::Step3Result;
use crate::step4_elevation::Step4Result;
use crate::step5_gravity_raster::GravityRaster;

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
/// `04_..._step2.svg`'s own ring around every position that contributed a
/// Heavy Object or Jump reading to Step 2's confidence-weighted vote -- see
/// [`vote_reading_points`].
const DARK_GREEN: Color = Color::Rgb(0, 100, 0);
/// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`'s
/// own contour-pixel potential-well force vector layer (Appendix 5) -- see
/// [`push_pull_vector_layers`].
const PUSH_PULL_CONTOUR_FORCE: Color = Color::Rgb(128, 0, 0);
/// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`'s
/// own `out_of_bound_force` vector layer.
const PUSH_PULL_OUT_OF_BOUND: Color = Color::Rgb(255, 0, 255);
/// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`'s
/// own `density_region_force` vector layer.
const PUSH_PULL_DENSITY: Color = Color::Rgb(0, 100, 0);
/// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`'s
/// own `flying_end_force` vector layer.
const PUSH_PULL_FLYING_END: Color = Color::Rgb(0, 191, 255);
/// `3` (high density) pixels.
const LIGHT_GREEN: Color = Color::Rgb(144, 238, 144);
/// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`'s
/// own integration-step dot layer -- a more saturated green than [`GREEN`]
/// (the un-grown linearized-contour layer) so the two read as distinct even
/// though both are "green".
const INTEGRATION_STEP_DOT: Color = Color::Rgb(0, 255, 0);

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
/// A Growing Process integration-step dot's radius, in ground meters --
/// smaller than [`DOT_RADIUS`], since one is drawn per integration step
/// taken and they can sit close together.
const INTEGRATION_STEP_DOT_RADIUS: f32 = 0.15;
/// A hysteresis marker's radius: slightly larger than [`DOT_RADIUS`] so it
/// peeks out from underneath a drop's own dot rather than being hidden by it.
const HYSTERESIS_DOT_RADIUS: f32 = DOT_RADIUS + 0.15;
/// A vote segment's stroke width, in ground meters.
const VOTE_SEGMENT_STROKE_WIDTH: f32 = 0.3;
/// A Slope Line's search-radius ring's stroke width, in ground meters: thin,
/// since it is reference context for `slope_lines_contours_search_radius`
/// rather than a reading itself.
const SEARCH_CIRCLE_STROKE_WIDTH: f32 = 0.15;
/// Close Search's own search-cone outline's stroke width, in ground meters
/// -- see [`write_step1_close_search_svg`].
const SEARCH_CONE_STROKE_WIDTH: f32 = 0.15;
/// A vote-reading ring's radius, in ground meters -- see
/// [`vote_reading_points`]. Small enough to mark a single point precisely
/// rather than dominate the picture, the same role
/// [`SEARCH_CIRCLE_STROKE_WIDTH`] plays for its own stroke.
const VOTE_CIRCLE_RADIUS: f32 = 1.0;
/// A Flying End's own pre-growing marker ring's radius, in ground meters.
const FLYING_END_RING_RADIUS: f32 = 1.5;
/// A Flying End marker ring's stroke width.
const FLYING_END_RING_STROKE_WIDTH: f32 = 0.3;
/// A drop's own trail's stroke width: a third of a vote segment's, since
/// it's background context for the drop's path rather than something to
/// emphasize the way an actual vote is.
const DROP_TRAIL_STROKE_WIDTH: f32 = VOTE_SEGMENT_STROKE_WIDTH / 3.0;
/// `08_..._step5_gravity.svg`'s own per-pixel gravity direction segment's
/// stroke width, in ground meters.
const GRAVITY_SEGMENT_STROKE_WIDTH: f32 = 0.2;
/// Below this averaged-direction magnitude (out of the `1.0` a single,
/// perfectly-agreeing contribution has -- see
/// [`crate::step5_gravity_raster::GravityRaster::get`]'s own doc comment on
/// why the raw magnitude is kept), a pixel's own gravity direction is
/// considered too undecided -- opposing tracks nearly cancelling out -- to
/// draw a segment for at all, rather than drawing one whose direction is
/// mostly noise.
const GRAVITY_DIRECTION_MIN_MAGNITUDE: f64 = 0.3;
/// How many discrete color buckets `08_..._step5_gravity.svg`'s own
/// gravity-direction segment layer quantizes its red ([`RED`], intense) to
/// blue ([`BLUE`], weak) magnitude gradient into: one `MultiLineString`
/// (one SVG element) drawn per bucket, rather than one individually-colored
/// element per pixel -- at a real map's own pixel count, the latter would
/// blow this file's size right back up to what [`pixel_layers`]'s own doc
/// comment already explains this whole sparse vector format exists to
/// avoid. High enough that the gradient still reads as continuous by eye.
const GRAVITY_INTENSITY_BUCKETS: usize = 20;
/// A push/pull vector's own stroke width, in ground meters -- see
/// [`push_pull_vector_layers`].
const PUSH_PULL_VECTOR_STROKE_WIDTH: f32 = 0.15;
/// How far, in ground meters, the viewBox is padded past the drawing's own
/// bounds so nothing is clipped at the edge.
const MARGIN: f32 = 2.0;
/// `07_..._step4.svg`'s own per-contour elevation-gradient line's stroke
/// width -- thicker than the linearized layer it's drawn over
/// ([`LINEARIZED_CONTOUR_STROKE_WIDTH`]), so the gradient color reads clearly
/// on top of it.
const STEP4_CONTOUR_STROKE_WIDTH: f64 = LINEARIZED_CONTOUR_STROKE_WIDTH as f64 * 2.0;
/// `07_..._step4.svg`'s own numeric elevation label font size, in ground
/// meters (matching every other size constant in this file).
const STEP4_LABEL_FONT_SIZE: f64 = 1.5;
/// `07_..._step4.svg`'s own tree-edge line's stroke width: thin, background
/// context for how elevation propagated rather than a reading itself.
const STEP4_TREE_EDGE_STROKE_WIDTH: f64 = DROP_TRAIL_STROKE_WIDTH as f64;
/// `07_..._step4.svg`'s own dead-end marker square's half-width, in ground
/// meters.
const STEP4_DEAD_END_MARKER_HALF_SIZE: f64 = 0.5;
/// `07_..._step4.svg`'s own flat color for a contour Step 4 ended without an
/// `elevation_height`.
const STEP4_UNDEFINED_COLOR: &str = "#808080";
/// `contours_function`'s own sampling step along `x`, in meters, per the
/// task that asked for this diagnostic.
const CONTOURS_FUNCTION_STEP: f64 = 0.1;
/// `contours_function`'s own curve/axis stroke width. Unrelated to any
/// ground-meter scale (this file plots a function, not a map), so a plain
/// small constant rather than one of the ground-meter widths above.
const CONTOURS_FUNCTION_STROKE_WIDTH: f32 = 0.05;

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

fn pixel_square(raster: &ContourRaster, px: usize, py: usize) -> Polygon<f64> {
    let x0 = raster.origin.x + px as f64 * raster.px_size;
    let y0 = raster.origin.y + py as f64 * raster.px_size;
    let (x1, y1) = (x0 + raster.px_size, y0 + raster.px_size);
    Polygon::new(
        LineString::new(vec![
            Coord { x: x0, y: y0 },
            Coord { x: x1, y: y0 },
            Coord { x: x1, y: y1 },
            Coord { x: x0, y: y1 },
            Coord { x: x0, y: y0 },
        ]),
        vec![],
    )
}

/// Every raster pixel worth a square in the vector output: a contour, or a
/// high-density one -- the two values that mark something the algorithm
/// actually resolved. `UNDEFINED`/`OUT_OF_BOUND`/`NO_CONTOUR_IN_BOUND` get no
/// square at all: drawing every single pixel, including the two or three
/// reserved values that typically cover most of a real map's own raster,
/// made these files huge. `OUT_OF_BOUND` gets its own single traced polygon
/// instead (see [`oob_area`]); `UNDEFINED`/`NO_CONTOUR_IN_BOUND` get nothing
/// at all (the grid lines alone show the raster's extent there). The full,
/// every-pixel-colored picture instead goes into the companion
/// `--create_svg` PNG (see [`crate::contour_raster::ContourRaster::write_png`]).
struct PixelLayers {
    contours: MultiPolygon<f64>,
    high_density: MultiPolygon<f64>,
}

fn pixel_layers(raster: &ContourRaster) -> PixelLayers {
    let (mut contours, mut high_density) = (Vec::new(), Vec::new());
    for py in 0..raster.height {
        for px in 0..raster.width {
            match raster.get(px as i64, py as i64) {
                v if v == HIGH_DENSITY => high_density.push(pixel_square(raster, px, py)),
                v if v < CONTOUR_0_MATRIX_VALUE => {} // UNDEFINED/OUT_OF_BOUND/NO_CONTOUR_IN_BOUND: no square
                _ => contours.push(pixel_square(raster, px, py)),
            }
        }
    }
    PixelLayers {
        contours: MultiPolygon::new(contours),
        high_density: MultiPolygon::new(high_density),
    }
}

/// A pixel-grid corner's world-space point -- `i` in `0..=raster.width`, `j`
/// in `0..=raster.height` -- the same corner convention [`pixel_square`]
/// already uses for a single pixel's own 4 corners.
fn corner(raster: &ContourRaster, i: i64, j: i64) -> Coord<f64> {
    Coord {
        x: raster.origin.x + i as f64 * raster.px_size,
        y: raster.origin.y + j as f64 * raster.px_size,
    }
}

/// A closed rectilinear ring's own corner-index vertices (first == last),
/// with every run of collinear points along a straight stretch collapsed
/// into just its own two ends -- an all-out-of-bound raster's own
/// single-pixel-wide zigzag would otherwise keep one point per pixel even
/// along a perfectly straight run. Falls back to the input unchanged if
/// collapsing it would leave nothing (never actually possible for a real
/// closed rectilinear ring, which always turns at at least 4 corners, but
/// cheaper to guard against than to prove impossible here).
fn simplify_rectilinear_ring(points: &[(i64, i64)]) -> Vec<(i64, i64)> {
    if points.len() < 4 {
        return points.to_vec();
    }
    let body = &points[..points.len() - 1];
    let n = body.len();
    let dir = |a: (i64, i64), b: (i64, i64)| (b.0 - a.0, b.1 - a.1);
    let mut kept: Vec<(i64, i64)> = (0..n)
        .filter(|&i| {
            let prev = body[(i + n - 1) % n];
            let cur = body[i];
            let next = body[(i + 1) % n];
            dir(prev, cur) != dir(cur, next)
        })
        .map(|i| body[i])
        .collect();
    if kept.is_empty() {
        return points.to_vec();
    }
    kept.push(kept[0]);
    kept
}

/// Every `OUT_OF_BOUND` pixel's own boundary, traced into a handful of
/// closed rings instead of one square per pixel (Appendix 5 -- see
/// [`pixel_layers`]'s own doc comment for why: on a real map, out-of-bound
/// is typically the *largest* pixel value by area, and a square per pixel
/// there made these files huge).
///
/// Every out-of-bound pixel is connected to the raster's own outer border
/// (`ContourRaster::compute_out_of_bound` starts there and only ever floods
/// outward from an already-out-of-bound pixel), so the
/// out-of-bound area is always exactly the raster's own full bounding
/// rectangle *minus* every "in bound" region found inside it. Rather than
/// work out which traced ring is a hole in which (arbitrarily nested, for a
/// map with islands within bays within islands), this returns the
/// bounding rectangle as one ring, plus one ring per out-of-bound/in-bound
/// transition found -- `--create_svg` renders them all as one `<path>` with
/// `fill-rule="evenodd"` ([`OobArea`]), which fills wherever an odd number
/// of rings cover a point, the correct out-of-bound area regardless of how
/// deep that nesting goes or which way each ring happens to wind.
///
/// Traced via directed unit edges around each out-of-bound pixel's own
/// exposed sides (skipped wherever the neighbor across that side is also
/// out-of-bound, including one that falls outside the raster entirely,
/// exactly like [`ContourRaster::get`]'s own semantics already treat it --
/// so no edge is ever placed on the raster's own true border, which the
/// explicit bounding-rectangle ring covers instead): every vertex this
/// produces has as many outgoing edges as incoming ones, so repeatedly
/// walking from any vertex with a remaining outgoing edge, consuming edges
/// as it goes, is guaranteed to return to that same vertex and close a
/// simple ring -- even where two out-of-bound pixels only touch at a shared
/// corner (diagonally, which `compute_out_of_bound`'s own 8-connected flood
/// allows), where the ring can end up touching itself at that one point
/// rather than crossing itself, which `fill-rule="evenodd"` still renders
/// correctly either way.
fn oob_area(raster: &ContourRaster) -> OobArea {
    let (w, h) = (raster.width as i64, raster.height as i64);
    if w == 0 || h == 0 {
        return OobArea(Vec::new());
    }

    let mut out_edges: HashMap<(i64, i64), Vec<(i64, i64)>> = HashMap::new();
    for y in 0..h {
        for x in 0..w {
            if raster.get(x, y) != OUT_OF_BOUND {
                continue;
            }
            if raster.get(x, y - 1) != OUT_OF_BOUND {
                out_edges.entry((x, y)).or_default().push((x + 1, y));
            }
            if raster.get(x + 1, y) != OUT_OF_BOUND {
                out_edges
                    .entry((x + 1, y))
                    .or_default()
                    .push((x + 1, y + 1));
            }
            if raster.get(x, y + 1) != OUT_OF_BOUND {
                out_edges
                    .entry((x + 1, y + 1))
                    .or_default()
                    .push((x, y + 1));
            }
            if raster.get(x - 1, y) != OUT_OF_BOUND {
                out_edges.entry((x, y + 1)).or_default().push((x, y));
            }
        }
    }

    let mut rings: Vec<Vec<(i64, i64)>> = vec![vec![(0, 0), (w, 0), (w, h), (0, h), (0, 0)]];
    for start in out_edges.keys().copied().collect::<Vec<_>>() {
        while out_edges.get(&start).is_some_and(|v| !v.is_empty()) {
            let mut ring = vec![start];
            let mut current = start;
            loop {
                let next = out_edges
                    .get_mut(&current)
                    .expect(
                        "in-degree == out-degree at every vertex this tracer touches, so a \
                         still-open ring can never get stuck at a non-start vertex",
                    )
                    .pop()
                    .expect("checked non-empty by the while condition above");
                ring.push(next);
                current = next;
                if current == start {
                    break;
                }
            }
            rings.push(ring);
        }
    }

    OobArea(
        rings
            .into_iter()
            .map(|ring| {
                LineString::new(
                    simplify_rectilinear_ring(&ring)
                        .into_iter()
                        .map(|(i, j)| corner(raster, i, j))
                        .collect(),
                )
            })
            .collect(),
    )
}

/// [`oob_area`]'s own traced rings, drawn as one `<path>` with
/// `fill-rule="evenodd"` rather than as ordinary `geo` polygons -- evenodd
/// fills wherever an odd number of rings cover a point, so the rings can be
/// emitted in any order, wound either way, without first working out which
/// nest inside which (see `oob_area`'s own doc comment).
struct OobArea(Vec<LineString<f64>>);

impl ToSvgStr for OobArea {
    fn to_svg_str(&self, style: &Style) -> String {
        let mut d = String::new();
        for ring in &self.0 {
            let Some(first) = ring.0.first() else {
                continue;
            };
            let _ = write!(d, "M {:?} {:?}", first.x, first.y);
            for p in &ring.0[1..] {
                let _ = write!(d, " L {:?} {:?}", p.x, p.y);
            }
            d.push_str(" Z ");
        }
        format!(r#"<path d="{d}"{style} fill-rule="evenodd"/>"#)
    }

    fn viewbox(&self, _style: &Style) -> ViewBox {
        self.0.iter().flatten().fold(ViewBox::default(), |vb, p| {
            vb.add(&ViewBox::new(
                p.x as f32, p.y as f32, p.x as f32, p.y as f32,
            ))
        })
    }
}

/// One unfilled ring per Flying End position, in red -- `Step1Result::pre_growing_flying_ends`,
/// drawn the same on `00_..._step1.svg` and both of the Growing Process's
/// own files, `02_..._step1_growing_seeking.svg`/
/// `03_..._step1_growing_matching.svg` (see the doc's Visualization
/// section).
fn flying_end_rings(result: &Step1Result) -> MultiPoint<f64> {
    MultiPoint::new(
        result
            .pre_growing_flying_ends
            .iter()
            .map(|&p| Point::from(p))
            .collect(),
    )
}

/// The post-linearization line of every contour the Growing Process did
/// *not* touch (green, `linearized_contour_lines`'s usual color) and every
/// one it did (blue) -- `02_..._step1_growing_seeking.svg`/
/// `03_..._step1_growing_matching.svg` (and everything built on them) show
/// grown/merged contours in blue instead of green, per the doc.
fn linearized_contour_lines_split(
    result: &Step1Result,
) -> (MultiLineString<f64>, MultiLineString<f64>) {
    let mut not_grown = Vec::new();
    let mut grown = Vec::new();
    for (idx, contour) in result.contours.iter().enumerate() {
        if result
            .grown_by_growing_process
            .get(idx)
            .copied()
            .unwrap_or(false)
        {
            grown.push(contour.lwg.ls.clone());
        } else {
            not_grown.push(contour.lwg.ls.clone());
        }
    }
    (MultiLineString::new(not_grown), MultiLineString::new(grown))
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

fn raw_contour_lines(result: &Step1Result) -> RawContours {
    RawContours(result.raw_polylines.clone())
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

/// One force contribution: tail at `from` (a Flying End's own position
/// *before* the integration step), head at `from + v * scale` -- unlike
/// [`arrow`], whose every caller already hands it a unit vector to stretch
/// to a fixed [`ARROW_LENGTH`], `v`'s own raw magnitude *is* the point here
/// (Appendix 5's own genuinely Newton-valued force), so it is scaled by
/// `growing_visualization_push_pull_vectors_scale` instead of normalized.
fn push_pull_vector(from: Coord<f64>, v: (f64, f64), scale: f64) -> LineString<f64> {
    LineString::new(vec![
        from,
        Coord {
            x: from.x + v.0 * scale,
            y: from.y + v.1 * scale,
        },
    ])
}

/// The four force vector layers `02_..._step1_growing_seeking.svg`/
/// `03_..._step1_growing_matching.svg` draw for every integration step
/// recorded in `Step1Result::growing_push_pull_vectors` --
/// one per force term (contour, out-of-bound, density, flying-end), all
/// sharing the same tail (that step's own pre-step Flying End position) but
/// scaled and colored separately per [`push_pull_vector`].
struct PushPullVectorLayers {
    contour: MultiLineString<f64>,
    out_of_bound: MultiLineString<f64>,
    density: MultiLineString<f64>,
    flying_end: MultiLineString<f64>,
}

fn push_pull_vector_layers(result: &Step1Result, scale: f64) -> PushPullVectorLayers {
    let (mut contour, mut out_of_bound, mut density, mut flying_end) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for forces in &result.growing_push_pull_vectors {
        contour.push(push_pull_vector(forces.flying_end, forces.contour, scale));
        out_of_bound.push(push_pull_vector(
            forces.flying_end,
            forces.out_of_bound,
            scale,
        ));
        density.push(push_pull_vector(forces.flying_end, forces.density, scale));
        flying_end.push(push_pull_vector(
            forces.flying_end,
            forces.flying_end_force,
            scale,
        ));
    }
    PushPullVectorLayers {
        contour: MultiLineString::new(contour),
        out_of_bound: MultiLineString::new(out_of_bound),
        density: MultiLineString::new(density),
        flying_end: MultiLineString::new(flying_end),
    }
}

/// Draws [`push_pull_vector_layers`]' four layers over `svg`, one color per
/// term (see the `PUSH_PULL_*` constants).
fn with_push_pull_vectors<'a>(svg: Svg<'a>, layers: &'a PushPullVectorLayers) -> Svg<'a> {
    svg.and(line_layer(
        &layers.contour,
        PUSH_PULL_CONTOUR_FORCE,
        PUSH_PULL_VECTOR_STROKE_WIDTH,
    ))
    .and(line_layer(
        &layers.out_of_bound,
        PUSH_PULL_OUT_OF_BOUND,
        PUSH_PULL_VECTOR_STROKE_WIDTH,
    ))
    .and(line_layer(
        &layers.density,
        PUSH_PULL_DENSITY,
        PUSH_PULL_VECTOR_STROKE_WIDTH,
    ))
    .and(line_layer(
        &layers.flying_end,
        PUSH_PULL_FLYING_END,
        PUSH_PULL_VECTOR_STROKE_WIDTH,
    ))
}

/// Every Jump's own buffered polygon (`LineGravityDefiners::poly`, built by
/// [`crate::contour_geometry::ls_to_polygon`]) -- the actual area Step 1's
/// `raster.pixels_in_polygon` scans to find which contours a Jump
/// intersects, so seeing it drawn is what lets a `heavy_object_width`/
/// `heavy_object_growing` choice be judged by eye against the real pixels.
fn line_definer_polygons(result: &Step1Result) -> MultiPolygon<f64> {
    MultiPolygon::new(
        result
            .line_definers
            .iter()
            .map(|definer| definer.poly.clone())
            .collect(),
    )
}

/// Every Heavy Object's own buffered polygon (`Step1Result::heavy_object_polygons`,
/// built the same way a Jump's is) -- the actual area Step 1 scans for
/// intersecting contours, so seeing it drawn is what lets a
/// `heavy_object_width`/`heavy_object_growing` choice be judged by eye
/// against the real pixels, the same as [`line_definer_polygons`] does for
/// Jumps.
fn heavy_object_polygons(result: &Step1Result) -> MultiPolygon<f64> {
    MultiPolygon::new(result.heavy_object_polygons.clone())
}

/// One arrow at a Jump's own definition point (`ls[0]`), in the direction
/// perpendicular to the Jump's line *there* -- via [`node_direction`], same
/// as [`contour_gravity_arrows`] draws a contour's, since a Jump's gravity
/// (like a contour's) is only ever meaningful relative to its own local
/// tangent, not a single vector valid along its whole length.
fn line_definer_arrows(result: &Step1Result) -> MultiLineString<f64> {
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
fn line_definer_span_arrows(result: &Step1Result) -> MultiLineString<f64> {
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
fn point_definer_arrows(result: &Step1Result) -> MultiLineString<f64> {
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
fn slope_line_circle_points(result: &Step1Result, resolved: bool) -> MultiPoint<f64> {
    MultiPoint::new(
        result
            .slope_lines
            .iter()
            .filter(|mark| mark.resolved == resolved)
            .map(|mark| Point::from(mark.pos))
            .collect(),
    )
}

/// Every position that contributed a Heavy Object or Jump reading to Step
/// 2's confidence-weighted vote (`step2_obvious_gravity::resolve`'s own
/// evidence-accumulation pass, over the exact same `point_definers`/
/// `line_definers` this function reads): a Heavy Object reading's own
/// `(x, y)`, or one of a Jump's own `touched_contours` centroids -- kept
/// regardless of whether the contour it touched ended up resolved by that
/// vote, a Slope Line, or the closed-hill heuristic, so every piece of
/// evidence the vote actually weighed stays visible, not just the evidence
/// that won.
fn vote_reading_points(result: &Step1Result) -> MultiPoint<f64> {
    let mut points = Vec::new();
    for definer in &result.point_definers {
        if definer.source != GravityReadingSource::HeavyObject {
            continue;
        }
        if definer.gravity_dx.is_none() || definer.gravity_dy.is_none() {
            continue;
        }
        points.push(Point::from(Coord {
            x: definer.x,
            y: definer.y,
        }));
    }
    for definer in &result.line_definers {
        if lwg_gravity_side(&definer.lwg).is_none() {
            continue;
        }
        for &(_, center) in &definer.touched_contours {
            points.push(Point::from(center));
        }
    }
    MultiPoint::new(points)
}

/// An arrow (same shape as [`point_definer_arrows`]'s, which already draws
/// one for every *resolved* Slope Line/Heavy Object reading) for every Slope
/// Line that did *not* resolve into a `PointGravityDefiners` reading,
/// computed directly from its own rotation since there is no resolved
/// reading in `result.point_definers` to draw from otherwise -- so an
/// unresolved Slope Line's own intended direction is still visible, in
/// orange rather than [`point_definer_arrows`]'s red to mark it as
/// unconfirmed.
fn unresolved_slope_line_arrows(result: &Step1Result) -> MultiLineString<f64> {
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
fn contour_gravity_arrows(result: &Step1Result) -> MultiLineString<f64> {
    contour_gravity_arrows_filtered(result, None)
}

/// Same as [`contour_gravity_arrows`], but when `only` is given, skips any
/// contour whose index isn't `true` in it -- used by
/// [`write_step3_rain_svg`] to draw an arrow only for a contour Rain Drop
/// Production (or an earlier step) itself resolved. `result.contours` is
/// shared, mutated-in-place state: by the time any `--create_svg` file is
/// written it already holds gravity from every step that has run so far,
/// including Anti Rain Drop Production if it ran, so without this filter
/// the rain SVG would draw an arrow for a contour no rain drop ever
/// touched.
fn contour_gravity_arrows_filtered(
    result: &Step1Result,
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

/// A flat coordinate list -- `Step3Result`'s own `rain_hysteresis_points`/
/// `anti_rain_hysteresis_points`, already computed by
/// `step3_rain_drop::simulate_one_drop` itself, since only it knows which
/// points fell inside a hysteresis window -- turned into drawable points.
fn points(coords: &[Coord<f64>]) -> MultiPoint<f64> {
    MultiPoint::new(coords.iter().map(|&c| Point::from(c)).collect())
}

/// `Step3Result`'s own `rain_vote_segments`/`anti_rain_vote_segments` --
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

/// An unfilled search-radius ring per point, in `color`.
fn circle_layer(points: &MultiPoint<f64>, radius: f32, color: Color) -> Svg<'_> {
    points
        .to_svg()
        .with_radius(radius)
        .with_fill_opacity(0.0)
        .with_stroke_color(color)
        .with_stroke_width(SEARCH_CIRCLE_STROKE_WIDTH)
}

/// Everything [`base_layers`] needs, computed once from a [`Step1Result`] so
/// every `write_step*_svg` function just builds one of these instead of ten
/// separate local variables.
struct BaseLayerData {
    oob: OobArea,
    grid: MultiLineString<f64>,
    pixels: PixelLayers,
    line_polygons: MultiPolygon<f64>,
    heavy_object_polygons: MultiPolygon<f64>,
    raw: RawContours,
    linearized: MultiLineString<f64>,
    grown_linearized: MultiLineString<f64>,
    line_arrows: MultiLineString<f64>,
    line_span_arrows: MultiLineString<f64>,
    point_arrows: MultiLineString<f64>,
    resolved_circles: MultiPoint<f64>,
    unresolved_circles: MultiPoint<f64>,
    unresolved_arrows: MultiLineString<f64>,
    flying_ends: MultiPoint<f64>,
    circle_radius: f32,
}

impl BaseLayerData {
    fn new(result: &Step1Result) -> BaseLayerData {
        let (linearized, grown_linearized) = linearized_contour_lines_split(result);
        BaseLayerData {
            oob: oob_area(&result.raster),
            grid: grid_lines(&result.raster),
            pixels: pixel_layers(&result.raster),
            line_polygons: line_definer_polygons(result),
            heavy_object_polygons: heavy_object_polygons(result),
            raw: raw_contour_lines(result),
            linearized,
            grown_linearized,
            line_arrows: line_definer_arrows(result),
            line_span_arrows: line_definer_span_arrows(result),
            point_arrows: point_definer_arrows(result),
            resolved_circles: slope_line_circle_points(result, true),
            unresolved_circles: slope_line_circle_points(result, false),
            unresolved_arrows: unresolved_slope_line_arrows(result),
            flying_ends: flying_end_rings(result),
            circle_radius: result.slope_lines_contours_search_radius as f32,
        }
    }
}

/// Draws [`oob_area`]'s own black polygon first, under every other layer
/// (a plain SVG paint order: everything drawn after it lands on top), then
/// the raw (red) contour layer before the linearized (green, or blue for a
/// contour the Growing Process touched) one -- later layers paint over
/// earlier ones in SVG, so this keeps the linearized line on top wherever
/// the two coincide, with the wider red line still peeking out on either
/// side.
fn base_layers(data: &BaseLayerData) -> Svg<'_> {
    data.oob
        .to_svg()
        .with_fill_color(BLACK)
        .with_stroke_opacity(0.0)
        .and(
            data.pixels
                .high_density
                .to_svg()
                .with_fill_color(LIGHT_GREEN)
                .with_stroke_opacity(0.0),
        )
        .and(
            data.pixels
                .contours
                .to_svg()
                .with_fill_color(BROWN)
                .with_fill_opacity(0.5)
                .with_stroke_opacity(0.0),
        )
        .and(
            data.line_polygons
                .to_svg()
                .with_fill_color(LIGHT_BLUE)
                .with_fill_opacity(0.4)
                .with_stroke_opacity(0.0),
        )
        .and(
            data.heavy_object_polygons
                .to_svg()
                .with_fill_color(LIGHT_PINK)
                .with_fill_opacity(0.4)
                .with_stroke_opacity(0.0),
        )
        .and(line_layer(&data.grid, GRAY, 0.05))
        .and(line_layer(&data.raw, RED, RAW_CONTOUR_STROKE_WIDTH))
        .and(line_layer(
            &data.linearized,
            GREEN,
            LINEARIZED_CONTOUR_STROKE_WIDTH,
        ))
        .and(line_layer(
            &data.grown_linearized,
            BLUE,
            LINEARIZED_CONTOUR_STROKE_WIDTH,
        ))
        .and(line_layer(&data.line_arrows, BLUE, 0.2))
        .and(line_layer(&data.line_span_arrows, PURPLE, 0.15))
        .and(line_layer(&data.point_arrows, RED, 0.2))
        .and(line_layer(&data.unresolved_arrows, ORANGE, 0.2))
        .and(circle_layer(
            &data.resolved_circles,
            data.circle_radius,
            RED,
        ))
        .and(circle_layer(
            &data.unresolved_circles,
            data.circle_radius,
            ORANGE,
        ))
        .and(
            data.flying_ends
                .to_svg()
                .with_radius(FLYING_END_RING_RADIUS)
                .with_fill_opacity(0.0)
                .with_stroke_color(RED)
                .with_stroke_width(FLYING_END_RING_STROKE_WIDTH),
        )
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

/// Writes the SVG asked for after Step 1's own Contour Raster (fill,
/// Jump density stamping, out-of-bound) is complete, but before its Growing
/// Process sub-step runs: the raster grid and every pixel (colored by
/// value), every contour raw and linearized, the Jump ("LineGravityDefiners")
/// arrows, the Slope Line ("PointGravityDefiners") arrows -- not yet a
/// Heavy Object's own, since `step1_extract::resolve_heavy_object_gravity`
/// (deferred until after Matching) hasn't run this early, though its own
/// buffered polygon is already drawn -- and a red ring around every Flying
/// End's own (pre-growing) position. `00_<map_name>_step1.svg`.
pub fn write_step1_svg(path: &Path, result: &Step1Result) -> Result<(), String> {
    write(path, base_layers(&BaseLayerData::new(result)))
}

/// Identical to [`write_step1_svg`], except called once after Step 1's own
/// Growing Process's preliminary Close Search pass has run
/// (`step1_extract::run_growing_close_search`): the Contour Raster and every
/// contour's `ls` reflect whatever Close Search resolved, and any contour it
/// touched draws blue instead of green there, the same
/// `result.grown_by_growing_process` flag `base_layers` always reads. On top
/// of everything else, its own topmost layer draws every search cone Close
/// Search actually used (`result.close_search_cones`, one per turn given,
/// see `search_cone_outline`) as an unfilled purple outline, so the area a
/// `searching_fov`/`searching_distance` choice actually searches can be
/// judged by eye against the real Flying Ends and pixels. Close Search takes
/// no integration steps of its own, so unlike [`write_step1_growing_svg`]
/// below, it has no push/pull vectors or dots to draw.
/// `01_<map_name>_step1_close_search.svg`.
pub fn write_step1_close_search_svg(path: &Path, result: &Step1Result) -> Result<(), String> {
    let cones = MultiLineString::new(result.close_search_cones.clone());
    write(
        path,
        base_layers(&BaseLayerData::new(result))
            .and(line_layer(&cones, PURPLE, SEARCH_CONE_STROKE_WIDTH)),
    )
}

/// Identical to [`write_step1_svg`] (including the same red Flying-End
/// rings, still at their original pre-growing positions), except called
/// once after each of Step 1's own Growing Process phases
/// (`step1_extract::run_growing_seeking`/`run_growing_matching`, Seeking
/// then Matching -- see the doc's Visualization section): the Contour Raster
/// and every contour's `ls` reflect however far growing has gotten by that
/// call, and any contour the Growing Process has touched so far, in either
/// phase, (grown, merged into, or both) is drawn in blue instead of green.
/// Every integration step's own four force contributions
/// (`Step1Result::growing_push_pull_vectors`, Appendix 5) are drawn as
/// separate colored vectors scaled by
/// `config.growing_visualization_push_pull_vectors_scale`, and every
/// integration step's own resulting position
/// (`Step1Result::growing_integration_step_dots`) is drawn as a small green
/// dot on top of everything else -- but only *that one phase's own* steps:
/// `run_growing_matching` starts both of those two fields fresh rather than
/// reclaiming Seeking's own, so `02_<map_name>_step1_growing_seeking.svg`
/// shows Seeking's own steps and `03_<map_name>_step1_growing_matching.svg`
/// shows only Matching's, never both overlaid on one picture. Also inherits
/// [`write_step1_svg`]'s own point-definer-arrow caveat above, but only
/// halfway through its two call sites: `02_<map_name>_step1_growing_seeking.svg`
/// (written right after `run_growing_seeking`) still shows no Heavy Object
/// arrow, but `03_<map_name>_step1_growing_matching.svg` does, since by then
/// `resolve_heavy_object_gravity` has already run (see
/// `src/bin/contours_to_raster.rs`'s own call ordering).
pub fn write_step1_growing_svg(
    path: &Path,
    result: &Step1Result,
    config: &Config,
) -> Result<(), String> {
    let push_pull =
        push_pull_vector_layers(result, config.growing_visualization_push_pull_vectors_scale);
    let dots = points(&result.growing_integration_step_dots);
    write(
        path,
        with_push_pull_vectors(base_layers(&BaseLayerData::new(result)), &push_pull).and(
            dots.to_svg()
                .with_radius(INTEGRATION_STEP_DOT_RADIUS)
                .with_fill_color(INTEGRATION_STEP_DOT)
                .with_stroke_opacity(0.0),
        ),
    )
}

/// [`base_layers`] plus a gravity arrow along every contour already
/// resolved -- what Step 2's own SVG shows, and what each of Step 3's two
/// files (rain, anti rain) build further on.
fn resolved_layers<'a>(
    data: &'a BaseLayerData,
    gravity_arrows: &'a MultiLineString<f64>,
) -> Svg<'a> {
    base_layers(data).and(line_layer(gravity_arrows, YELLOW, 0.2))
}

/// The same as [`write_step1_growing_svg`], plus a gravity arrow along every
/// contour Step 2 (or Step 1's direct evidence) has already resolved, plus
/// (this file only, not [`write_step3_rain_svg`]/[`write_step3_anti_rain_svg`]
/// even though both also call [`resolved_layers`]) a dark green ring around
/// every position that contributed a Heavy Object or Jump reading to Step 2's
/// vote (see [`vote_reading_points`]), so the evidence behind a
/// vote-resolved contour -- or a warning about evidence Step 2 overruled --
/// can be judged by eye. `04_<map_name>_step2.svg`.
pub fn write_step2_svg(path: &Path, result: &Step1Result) -> Result<(), String> {
    let data = BaseLayerData::new(result);
    let gravity_arrows = contour_gravity_arrows(result);
    let vote_points = vote_reading_points(result);
    write(
        path,
        resolved_layers(&data, &gravity_arrows).and(circle_layer(
            &vote_points,
            VOTE_CIRCLE_RADIUS,
            DARK_GREEN,
        )),
    )
}

/// Linearly interpolates each of `low`'s and `high`'s own RGB channels by
/// `t` (clamped to `[0, 1]`) -- `t = 0.0` is `low`, `t = 1.0` is `high`. Every
/// color this file defines is [`Color::Rgb`], so that's the only variant
/// handled.
fn lerp_color(low: Color, high: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (Color::Rgb(lr, lg, lb), Color::Rgb(hr, hg, hb)) = (low, high) else {
        unreachable!("every color this file defines is Color::Rgb");
    };
    Color::Rgb(lerp_u8(lr, hr, t), lerp_u8(lg, hg, t), lerp_u8(lb, hb, t))
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

/// One segment per in-bound pixel `gravity` gave a confidently-decided
/// direction to (see [`GRAVITY_DIRECTION_MIN_MAGNITUDE`]): tail at the
/// pixel's own center, head half a pixel further along the pixel's own
/// (normalized) gravity direction -- long enough to read as an arrow field at
/// a glance without one pixel's own segment overlapping its neighbor's.
/// Grouped into [`GRAVITY_INTENSITY_BUCKETS`] magnitude buckets, index `0`
/// (weakest ever drawn, [`BLUE`]) to `GRAVITY_INTENSITY_BUCKETS - 1`
/// (strongest possible, [`RED`]) -- bucketed by magnitude *rescaled* against
/// the `[GRAVITY_DIRECTION_MIN_MAGNITUDE, 1.0]` range this function actually
/// draws (rather than the raw `[0.0, 1.0]` a pixel's magnitude can take, see
/// [`crate::step5_gravity_raster::GravityRaster::get`]'s own doc comment on
/// why it's never negative or above `1.0`), so the full blue-to-red gradient
/// is actually visible across what's drawn instead of every pixel bunching
/// into the reddish end of a range whose own bluest 30% (below the
/// threshold) is never drawn at all.
fn gravity_direction_segments(
    raster: &ContourRaster,
    gravity: &GravityRaster,
) -> Vec<MultiLineString<f64>> {
    let half_px = raster.px_size / 2.0;
    let mut buckets: Vec<Vec<LineString<f64>>> = vec![Vec::new(); GRAVITY_INTENSITY_BUCKETS];
    for y in 0..gravity.height {
        for x in 0..gravity.width {
            let Some((vx, vy)) = gravity.get(x, y) else {
                continue;
            };
            let magnitude = vx.hypot(vy);
            if magnitude < GRAVITY_DIRECTION_MIN_MAGNITUDE {
                continue;
            }
            let intensity =
                (magnitude - GRAVITY_DIRECTION_MIN_MAGNITUDE) / (1.0 - GRAVITY_DIRECTION_MIN_MAGNITUDE);
            let center = raster.pixel_center(x as i64, y as i64);
            let (ux, uy) = (vx / magnitude, vy / magnitude);
            let line = LineString::new(vec![
                center,
                Coord {
                    x: center.x + ux * half_px,
                    y: center.y + uy * half_px,
                },
            ]);
            let bucket = ((intensity.clamp(0.0, 1.0) * GRAVITY_INTENSITY_BUCKETS as f64) as usize)
                .min(GRAVITY_INTENSITY_BUCKETS - 1);
            buckets[bucket].push(line);
        }
    }
    buckets.into_iter().map(MultiLineString::new).collect()
}

/// Step 5's own Gravity Direction sub-step, visualized: [`base_layers`] (the
/// raster grid, brown contour pixels, green high-density area, pink heavy
/// object area -- the same picture `00_..._step1.svg` through
/// `04_..._step2.svg` already draw), plus one segment per pixel showing
/// [`crate::step5_gravity_raster::resolve`]'s own per-pixel gravity
/// direction, colored by its own magnitude -- [`RED`] for the most
/// confidently-decided pixels, fading to [`BLUE`] for the least (see
/// [`gravity_direction_segments`]). Diagnostic only for now -- there is no
/// TIFF or further use of `gravity` yet, unlike Step 5's own altitude
/// ([`crate::step5_elevation_raster`]). `08_<map_name>_step5_gravity.svg`.
pub fn write_step5_gravity_svg(
    path: &Path,
    result: &Step1Result,
    gravity: &GravityRaster,
) -> Result<(), String> {
    let data = BaseLayerData::new(result);
    let buckets = gravity_direction_segments(&result.raster, gravity);
    let mut svg = base_layers(&data);
    for (i, bucket) in buckets.iter().enumerate() {
        let t = (i as f64 + 0.5) / GRAVITY_INTENSITY_BUCKETS as f64;
        let color = lerp_color(BLUE, RED, t);
        svg = svg.and(line_layer(bucket, color, GRAVITY_SEGMENT_STROKE_WIDTH));
    }
    write(path, svg)
}

/// The algorithm's actual answer, once every contour's gravity is settled:
/// pixels, both contour layers (raw red under linearized green/blue), and
/// one gravity arrow per node (yellow) -- no raster grid, no Jump-only
/// definer arrows, no vote-reading rings, and none of Step 3's own
/// rain-drop-path layers, since those are per-step working detail rather
/// than the final picture. Call this only after every contour is resolved
/// (Step 2 alone, or
/// Step 2 and Step 3 together) -- an earlier call would just draw whatever
/// gravity happens to be set so far, silently mislabeled as final.
pub fn write_final_svg(path: &Path, result: &Step1Result) -> Result<(), String> {
    let pixels = pixel_layers(&result.raster);
    let raw = raw_contour_lines(result);
    let (linearized, grown_linearized) = linearized_contour_lines_split(result);
    let gravity_arrows = contour_gravity_arrows(result);
    write(
        path,
        pixels
            .contours
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
            .and(line_layer(
                &grown_linearized,
                BLUE,
                LINEARIZED_CONTOUR_STROKE_WIDTH,
            ))
            .and(line_layer(&gravity_arrows, YELLOW, 0.2)),
    )
}

/// The same as [`write_step2_svg`], plus every Rain Drop Production drop's
/// full trail (gray, thin -- background context so the drop's actual path
/// reads as a line rather than a scatter of dots), its own path (blue dots,
/// over a black marker under any point `step3.rain_hysteresis_points` names
/// as still inside a hysteresis window, and a purple segment over that for
/// each (previous position, current position) step `step3.rain_vote_segments`
/// names as where it actually cast a vote -- a vote is a property of the
/// step it happened on, not of either endpoint, so it is drawn as the step
/// itself rather than a dot that would otherwise just sit on top of the
/// hysteresis marker) -- kept to its own file, apart from
/// [`write_step3_anti_rain_svg`]'s, so a drop's path stays legible where the
/// two passes cross rather than overlaying both colors on one picture. Its
/// gravity-arrow layer is filtered to `step3.defined_after_rain`, so it
/// never shows an arrow for a contour only the final, combined rain +
/// anti rain vote tally went on to resolve, even though `result.contours`
/// itself already holds that final state by the time this runs -- gravity is
/// no longer assigned right after the rain pass alone (see
/// `step3_rain_drop::resolve`), so in practice `defined_after_rain` is
/// always identical to whatever was already defined before Step 3 even
/// started. `05_<map_name>_step3_rain.svg`.
pub fn write_step3_rain_svg(
    path: &Path,
    result: &Step1Result,
    step3: &Step3Result,
) -> Result<(), String> {
    let data = BaseLayerData::new(result);
    let gravity_arrows = contour_gravity_arrows_filtered(result, Some(&step3.defined_after_rain));
    let trails = drop_trails(&step3.rain_paths);
    let hysteresis_marks = points(&step3.rain_hysteresis_points);
    let vote_lines = segments(&step3.rain_vote_segments);
    let rain = drop_points(&step3.rain_paths);
    write(
        path,
        resolved_layers(&data, &gravity_arrows)
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

/// The same as [`write_step2_svg`], plus every Anti Rain Drop Production
/// drop's full trail, path, hysteresis marker and vote segment, the same
/// way as [`write_step3_rain_svg`] -- see there for why this is a separate
/// file rather than a second layer on the same one.
/// `06_<map_name>_step3_anti_rain.svg`.
pub fn write_step3_anti_rain_svg(
    path: &Path,
    result: &Step1Result,
    step3: &Step3Result,
) -> Result<(), String> {
    let data = BaseLayerData::new(result);
    let gravity_arrows = contour_gravity_arrows(result);
    let trails = drop_trails(&step3.anti_rain_paths);
    let hysteresis_marks = points(&step3.anti_rain_hysteresis_points);
    let vote_lines = segments(&step3.anti_rain_vote_segments);
    let anti_rain = drop_points(&step3.anti_rain_paths);
    write(
        path,
        resolved_layers(&data, &gravity_arrows)
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

/// Linearly interpolates a blue (`min_h`, or below)-to-red (`max_h`, or
/// above) gradient color for `h`, as a `#rrggbb` hex string -- `geo_svg`'s
/// own layer coloring is one color per whole layer, so [`step4_overlay_svg`]
/// writes this raw rather than through the `Svg`/`ToSvg` machinery every
/// other layer in this file uses. `min_h == max_h` (every resolved contour at
/// the same height) falls back to the gradient's own midpoint color rather
/// than dividing by zero.
fn height_gradient_hex(h: f64, min_h: f64, max_h: f64) -> String {
    let t = if (max_h - min_h).abs() < 1e-9 {
        0.5
    } else {
        ((h - min_h) / (max_h - min_h)).clamp(0.0, 1.0)
    };
    let r = (t * 255.0).round() as u8;
    let b = ((1.0 - t) * 255.0).round() as u8;
    format!("#{r:02x}00{b:02x}")
}

/// The point `07_..._step4.svg` labels/connects/marks a contour at: the
/// point exactly halfway along its own `ls` by arc length, guaranteed to sit
/// *on* the contour's own drawn line -- unlike a geometric centroid (the
/// length-weighted mean of every segment's own midpoint), which for a closed
/// ring sits near the ring's own enclosed area instead. That difference is
/// invisible for one lone contour, but concentric elevation contours (the
/// ordinary case for a real hill) all share roughly the same enclosed-area
/// center, so their centroids -- and every label/marker built from them --
/// would otherwise collapse into a single, illegible cluster regardless of
/// how different in size the rings themselves actually are. `None` only for
/// a degenerate (`< 2`-point) `ls`.
fn contour_label_point(ls: &LineString<f64>) -> Option<Point<f64>> {
    Euclidean.point_at_ratio_from_start(ls, 0.5)
}

/// `07_..._step4.svg`'s own raw overlay (see [`write_step4_svg`]): a
/// gradient-colored `<polyline>` (or solid [`STEP4_UNDEFINED_COLOR`] for a
/// contour that ended Step 4 without an `elevation_height`) and a `<text>`
/// label at its own [`contour_label_point`] naming its own index into
/// `contours` -- `"<idx>: <height>"` for a resolved one, just `"<idx>"`
/// (still visible, so any contour named in a warning, like Step 4's own
/// "contour N is a descendant of contour M", can actually be found in the
/// picture) for one that isn't; one gray `<line>` per `step4.tree_edges`
/// entry, parent's label point to child's; and one black `<rect>` per
/// `step4.dead_ends` entry, centered on that contour's own label point.
fn step4_overlay_svg(contours: &[Contour], step4: &Step4Result) -> String {
    let (min_h, max_h) = contours
        .iter()
        .filter_map(|c| c.elevation_height)
        .fold(None, |acc: Option<(f64, f64)>, h| {
            Some(acc.map_or((h, h), |(lo, hi)| (lo.min(h), hi.max(h))))
        })
        .unwrap_or((0.0, 0.0));

    let mut out = String::new();
    for (idx, c) in contours.iter().enumerate() {
        let color = match c.elevation_height {
            Some(h) => height_gradient_hex(h, min_h, max_h),
            None => STEP4_UNDEFINED_COLOR.to_string(),
        };
        let points: String = c
            .lwg
            .ls
            .0
            .iter()
            .map(|p| format!("{},{}", p.x, p.y))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = write!(
            out,
            r#"<polyline points="{points}" fill="none" stroke="{color}" stroke-width="{STEP4_CONTOUR_STROKE_WIDTH}"/>"#
        );
        if let Some(label_point) = contour_label_point(&c.lwg.ls) {
            let label = match c.elevation_height {
                Some(h) => format!("{idx}: {h:.0}"),
                None => idx.to_string(),
            };
            let _ = write!(
                out,
                r#"<text x="{}" y="{}" font-size="{STEP4_LABEL_FONT_SIZE}" fill="black">{label}</text>"#,
                label_point.x(),
                label_point.y(),
            );
        }
    }

    for &(parent_idx, child_idx) in &step4.tree_edges {
        if let (Some(p), Some(ch)) = (
            contour_label_point(&contours[parent_idx].lwg.ls),
            contour_label_point(&contours[child_idx].lwg.ls),
        ) {
            let _ = write!(
                out,
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="gray" stroke-width="{STEP4_TREE_EDGE_STROKE_WIDTH}"/>"#,
                p.x(),
                p.y(),
                ch.x(),
                ch.y(),
            );
        }
    }

    for &idx in &step4.dead_ends {
        if let Some(label_point) = contour_label_point(&contours[idx].lwg.ls) {
            let half = STEP4_DEAD_END_MARKER_HALF_SIZE;
            let _ = write!(
                out,
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="black"/>"#,
                label_point.x() - half,
                label_point.y() - half,
                half * 2.0,
                half * 2.0,
            );
        }
    }

    out
}

/// `07_<map_name>_step4.svg`: the same base layer as
/// `06_<map_name>_step3_anti_rain.svg` (grid, pixels, polygons, both
/// contour-line layers) -- no rain-drop-path layers, no gravity arrows -- with
/// every contour redrawn on top per [`step4_overlay_svg`]: gradient-colored
/// by `elevation_height` (blue lowest to red highest) with a numeric label,
/// gray for one Step 4 leaves without a height; a gray line per `T` edge
/// (parent centroid to child centroid); and a black square on every
/// `empty_progeny` dead end.
pub fn write_step4_svg(
    path: &Path,
    result: &Step1Result,
    step4: &Step4Result,
) -> Result<(), String> {
    let data = BaseLayerData::new(result);
    let mut svg = finish(base_layers(&data));
    let insert_at = svg
        .rfind("</svg>")
        .ok_or_else(|| format!("{}: malformed SVG, no closing </svg> tag", path.display()))?;
    svg.insert_str(insert_at, &step4_overlay_svg(&result.contours, step4));
    fs::write(path, svg).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `contour_force_magnitude`'s own curve (Appendix 5): `x` the distance from
/// a single contour/`TEMPORARY_CONTOUR` pixel in meters, `y` that pixel's
/// resulting force -- sampled every `CONTOURS_FUNCTION_STEP` meters from `0`
/// to `2 * contour_force_second_equilibrium`, twice the distance at which
/// the curve settles at its own `contour_force_max_attraction` plateau, so
/// the plateau itself is visible rather than just the point it starts at.
fn contours_function_curve(config: &Config) -> LineString<f64> {
    let max_x = 2.0 * config.contour_force_second_equilibrium;
    let steps = (max_x / CONTOURS_FUNCTION_STEP).round() as i64;
    LineString::new(
        (0..=steps)
            .map(|i| {
                let x = i as f64 * CONTOURS_FUNCTION_STEP;
                Coord {
                    x,
                    y: contour_force_magnitude(x, config),
                }
            })
            .collect(),
    )
}

/// Writes `<map_name>_contours_function.svg`: a plain 2D plot of
/// [`contour_force_magnitude`]'s own potential-well curve (Appendix 5) --
/// not a map, so no grid/pixels/out-of-bound layer, and its axes are the
/// function's own domain/range rather than ground positions. The curve is
/// drawn maroon, matching `02_<map_name>_step1_growing_seeking.svg`/
/// `03_<map_name>_step1_growing_matching.svg`'s own contour-pixel push/pull
/// vector layer, over a gray `y = 0` line (the
/// x-axis) and a gray `x = 0` line spanning the curve's own min/max (the
/// y-axis), so the repulsion/attraction crossover at
/// `contour_force_equilibrium` and the attraction plateau at
/// `contour_force_second_equilibrium` both read at a glance. Depends only on
/// `config`, unlike every other `write_*_svg` here, so it is written once
/// per `--create_svg` run regardless of the map or how far Step 1-3 got.
pub fn write_contours_function_svg(path: &Path, config: &Config) -> Result<(), String> {
    let curve = contours_function_curve(config);
    let max_x = 2.0 * config.contour_force_second_equilibrium;
    let (min_y, max_y) = curve
        .0
        .iter()
        .fold((0.0_f64, 0.0_f64), |(lo, hi), c| (lo.min(c.y), hi.max(c.y)));
    let x_axis = LineString::new(vec![Coord { x: 0.0, y: 0.0 }, Coord { x: max_x, y: 0.0 }]);
    let y_axis = LineString::new(vec![
        Coord { x: 0.0, y: min_y },
        Coord { x: 0.0, y: max_y },
    ]);
    write(
        path,
        line_layer(&x_axis, GRAY, CONTOURS_FUNCTION_STROKE_WIDTH)
            .and(line_layer(&y_axis, GRAY, CONTOURS_FUNCTION_STROKE_WIDTH))
            .and(line_layer(
                &curve,
                PUSH_PULL_CONTOUR_FORCE,
                CONTOURS_FUNCTION_STROKE_WIDTH,
            )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour_raster::ContourRaster;
    use crate::gravity_model::{
        gravity_vector_for_side, Contour, GravityReadingSource, LineGravityDefiners,
        LineWithGravity, PointGravityDefiners,
    };
    use crate::step1_extract::{GrowingStepForces, SlopeLineMark};
    use geo::Contains;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn v(x: f64, y: f64, is_curve_start: bool) -> RawVertex {
        RawVertex {
            coord: c(x, y),
            is_curve_start,
        }
    }

    fn sample_result() -> Step1Result {
        let ls = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0), c(10.0, 0.0)]);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ls.clone()),
            elevation_height: None,
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(1.0);

        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 12, 3);
        raster.write_contour(0, &ls);

        Step1Result {
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
            pre_growing_flying_ends: Vec::new(),
            close_search_cones: Vec::new(),
            grown_by_growing_process: vec![false],
            growing_push_pull_vectors: Vec::new(),
            growing_integration_step_dots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn grid_lines_cover_every_row_and_column() {
        let raster = ContourRaster::new(c(0.0, 0.0), 1.0, 4, 3);
        assert_eq!(grid_lines(&raster).0.len(), (4 + 1) + (3 + 1));
    }

    #[test]
    fn pixel_layers_only_squares_contours_and_high_density() {
        let result = sample_result();
        // The 10m-long horizontal contour at y=0 through a raster whose
        // origin is (-1, -1): its own pixels show up in the contour layer;
        // the rest of the (all-undefined) grid gets no square at all.
        let layers = pixel_layers(&result.raster);
        assert!(!layers.contours.0.is_empty());
        assert!(layers.high_density.0.is_empty());
        let total_squares = layers.contours.0.len() + layers.high_density.0.len();
        assert!(
            total_squares < result.raster.width * result.raster.height,
            "expected far fewer squares than raster pixels"
        );
    }

    #[test]
    fn simplify_rectilinear_ring_keeps_only_the_corners_of_a_straight_run() {
        // A closed rectangle whose left and right sides are each split into
        // 3 collinear pieces -- only the 4 true corners should survive.
        let ring = vec![(0, 0), (0, 1), (0, 2), (2, 2), (2, 1), (2, 0), (0, 0)];
        assert_eq!(
            simplify_rectilinear_ring(&ring),
            vec![(0, 0), (0, 2), (2, 2), (2, 0), (0, 0)]
        );
    }

    #[test]
    fn oob_area_traces_the_border_and_a_hole_for_an_enclosed_in_bound_region() {
        // A closed ring of contour pixels a couple of pixels in from the
        // border blocks `compute_out_of_bound`'s own flood (same fixture as
        // `contour_raster`'s own
        // `compute_out_of_bound_is_blocked_by_a_ring_of_contour_pixels`):
        // the interior it encloses stays undefined, i.e. "in bound", and
        // must come back as its own hole ring.
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 11, 11);
        raster.write_contour(
            0,
            &LineString::new(vec![
                c(2.5, 2.5),
                c(8.5, 2.5),
                c(8.5, 8.5),
                c(2.5, 8.5),
                c(2.5, 2.5),
            ]),
        );
        raster.compute_out_of_bound();

        let area = oob_area(&raster);
        assert_eq!(
            area.0.len(),
            2,
            "the raster's own bounding rectangle, plus one ring for the enclosed region"
        );
        for ring in &area.0 {
            assert_eq!(
                ring.0.first(),
                ring.0.last(),
                "every ring must be closed: {ring:?}"
            );
        }

        let rings_containing = |pt: Coord<f64>| -> usize {
            area.0
                .iter()
                .filter(|ring| Polygon::new((*ring).clone(), vec![]).contains(&pt))
                .count()
        };
        // Deep inside the enclosed region: covered by the bounding
        // rectangle *and* the hole ring -- an even count, so
        // `fill-rule="evenodd"` leaves it unfilled (correctly "in bound").
        assert_eq!(rings_containing(c(5.5, 5.5)) % 2, 0);
        // A corner pixel, genuinely out of bound: covered by the bounding
        // rectangle alone -- odd, so evenodd fills it.
        assert_eq!(rings_containing(c(0.5, 0.5)) % 2, 1);
    }

    #[test]
    fn oob_area_handles_a_diagonally_touching_pinch_without_panicking() {
        // Protects pixels (1, 1) and (2, 2) as non-out-of-bound contour
        // pixels; (1, 2) and (2, 1) are left undefined and get flooded to
        // out-of-bound from the border via `compute_out_of_bound`'s own
        // 8-connectedness -- leaving two out-of-bound pixels that touch
        // only diagonally, at the one corner shared by all 4, the exact
        // ambiguous case `oob_area`'s own boundary tracer must decompose
        // into simple (if self-touching) rings rather than choke on.
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 4, 4);
        raster.write_contour(0, &LineString::new(vec![c(1.5, 1.5), c(1.9, 1.5)]));
        raster.write_contour(1, &LineString::new(vec![c(2.5, 2.5), c(2.9, 2.5)]));
        raster.compute_out_of_bound();

        assert_eq!(raster.get(1, 1), CONTOUR_0_MATRIX_VALUE);
        assert_eq!(raster.get(2, 2), CONTOUR_0_MATRIX_VALUE + 1);
        assert_eq!(raster.get(1, 2), OUT_OF_BOUND);
        assert_eq!(raster.get(2, 1), OUT_OF_BOUND);

        let area = oob_area(&raster); // must not panic
        assert!(!area.0.is_empty());
        for ring in &area.0 {
            assert_eq!(ring.0.first(), ring.0.last());
        }
    }

    fn sample_config() -> Config {
        Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            step2_vote_min_total_weight: 0.3,
            step2_vote_min_margin: 0.15,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            elevation_vote_min_total_weight: 0.3,
            elevation_vote_min_margin: 0.15,
            growing_enabled: 1.0,
            obvious_to_close_contour_distance: 0.0,
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 4.0,
            attraction_force_window: 4.0,
            contour_force_max_repulsion: 2.0,
            contour_force_equilibrium: 1.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 3.0,
            out_of_bound_force: 0.5,
            density_region_force: 1.0,
            flying_end_force: 1.0,
            flying_end_merge_distance: 0.5,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
            growing_visualization_push_pull_vectors_scale: 2.0,
            gravity_gaussian_kernel_size: 5,
        }
    }

    #[test]
    fn push_pull_vector_layers_scale_each_contribution_from_its_own_flying_end() {
        let mut result = sample_result();
        result.growing_push_pull_vectors = vec![GrowingStepForces {
            flying_end: c(3.0, 4.0),
            contour: (1.0, 0.0),
            out_of_bound: (0.0, 2.0),
            density: (0.0, 0.0),
            flying_end_force: (-1.0, -1.0),
        }];
        let layers = push_pull_vector_layers(&result, 2.0);
        assert_eq!(layers.contour.0[0].0[0], c(3.0, 4.0));
        assert_eq!(layers.contour.0[0].0[1], c(5.0, 4.0));
        assert_eq!(layers.out_of_bound.0[0].0[1], c(3.0, 8.0));
        assert_eq!(
            layers.density.0[0].0[1],
            c(3.0, 4.0),
            "a zero contribution is a zero-length vector, not skipped"
        );
        assert_eq!(layers.flying_end.0[0].0[1], c(1.0, 2.0));
    }

    #[test]
    fn step1_growing_svg_draws_one_colored_vector_per_push_pull_contribution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1_growing.svg");
        let mut result = sample_result();
        result.growing_push_pull_vectors = vec![GrowingStepForces {
            flying_end: c(3.0, 4.0),
            contour: (1.0, 0.0),
            out_of_bound: (0.0, 1.0),
            density: (1.0, 1.0),
            flying_end_force: (-1.0, 0.0),
        }];
        write_step1_growing_svg(&path, &result, &sample_config()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        for needle in [
            "rgb(128,0,0)",   // PUSH_PULL_CONTOUR_FORCE
            "rgb(255,0,255)", // PUSH_PULL_OUT_OF_BOUND
            "rgb(0,100,0)",   // PUSH_PULL_DENSITY
            "rgb(0,191,255)", // PUSH_PULL_FLYING_END
        ] {
            assert!(
                text.contains(needle),
                "expected a layer stroked {needle}; got: {text}"
            );
        }
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
        let path = dir.path().join("step1.svg");
        let result = sample_result();
        write_step1_svg(&path, &result).unwrap();
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
    fn step1_svg_has_contours_and_no_gravity_arrows_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
        let result = sample_result();
        write_step1_svg(&path, &result).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("<svg"));
        assert!(text.trim_end().ends_with("</svg>"));
        assert_eq!(count(&text, "rgb(220,20,60)"), 1); // one raw contour segment
        assert_eq!(count(&text, "rgb(34,139,34)"), 1); // one linearized contour segment
        assert_eq!(count(&text, "rgb(230,200,20)"), 0); // no gravity arrows yet
    }

    #[test]
    fn step2_svg_adds_one_gravity_arrow_per_node() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2.svg");
        let result = sample_result();
        write_step2_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count(&text, "rgb(230,200,20)"), 3);
    }

    #[test]
    fn step2_svg_draws_a_dark_green_ring_at_each_vote_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step2.svg");
        let mut result = sample_result();
        result.point_definers = vec![
            // A Heavy Object reading: counts as one vote-reading position.
            PointGravityDefiners {
                x: 3.0,
                y: 7.0,
                reference_contour: 0,
                gravity_dx: Some(1.0),
                gravity_dy: Some(0.0),
                source: GravityReadingSource::HeavyObject,
            },
            // A Slope Line reading never feeds the vote, so it draws no ring.
            PointGravityDefiners {
                x: -3.0,
                y: -7.0,
                reference_contour: 0,
                gravity_dx: Some(1.0),
                gravity_dy: Some(0.0),
                source: GravityReadingSource::SlopeLine,
            },
        ];
        let jump_ls = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0)]);
        let mut lwg = LineWithGravity::new(jump_ls);
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
        result.line_definers = vec![LineGravityDefiners {
            lwg,
            poly,
            // Two touched contours: counts as two more vote-reading positions.
            touched_contours: vec![(0, c(1.0, 0.0)), (0, c(4.0, 0.0))],
        }];
        write_step2_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // One Heavy Object reading plus two Jump touched-contour centroids;
        // the Slope Line reading above draws no ring of its own.
        assert_eq!(count(&text, "rgb(0,100,0)"), 3);
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
                source: GravityReadingSource::SlopeLine,
            },
            // No gravity reading was derived for this one -- it draws no arrow.
            PointGravityDefiners {
                x: 5.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: None,
                gravity_dy: None,
                source: GravityReadingSource::SlopeLine,
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
    fn step1_svg_draws_a_red_point_definer_arrow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
        let mut result = sample_result();
        result.point_definers = vec![PointGravityDefiners {
            x: 3.0,
            y: 7.0,
            reference_contour: 0,
            gravity_dx: Some(1.0),
            gravity_dy: Some(0.0),
            source: GravityReadingSource::SlopeLine,
        }];
        write_step1_svg(&path, &result).unwrap();
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
    fn step1_svg_colors_slope_line_circles_and_arrows_by_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
        let mut result = sample_result();
        // A resolved reading also needs a matching point_definers entry --
        // that's what actually draws its (red) arrow, mirroring how Step 1
        // itself only ever produces a resolved SlopeLineMark alongside one.
        result.point_definers = vec![PointGravityDefiners {
            x: 3.0,
            y: 7.0,
            reference_contour: 0,
            gravity_dx: Some(0.0),
            gravity_dy: Some(-1.0),
            source: GravityReadingSource::SlopeLine,
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
        write_step1_svg(&path, &result).unwrap();
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
    fn step1_svg_fills_a_line_definer_polygon_light_blue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
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
        result.line_definers = vec![LineGravityDefiners {
            lwg,
            poly,
            touched_contours: Vec::new(),
        }];
        write_step1_svg(&path, &result).unwrap();
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
    fn line_definer_span_arrows_are_purple_not_yellow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
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
        result.line_definers = vec![LineGravityDefiners {
            lwg,
            poly,
            touched_contours: Vec::new(),
        }];
        write_step1_svg(&path, &result).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(count(&text, "rgb(148,0,211)") >= 1, "{text}");
        // No gravity-resolved contour is drawn in write_step1_svg, so this
        // Jump's own span arrows are the only thing that could draw yellow.
        assert_eq!(count(&text, "rgb(230,200,20)"), 0);
    }

    #[test]
    fn step1_svg_fills_a_heavy_object_polygon_light_pink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step1.svg");
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
        write_step1_svg(&path, &result).unwrap();
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

    fn result_for(ls: LineString<f64>, side: f64) -> Step1Result {
        let (gx, gy) = gravity_vector_for_side(&ls, side);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(gx);
        contour.lwg.gravity_dy = Some(gy);
        Step1Result {
            contours: vec![contour],
            raw_polylines: vec![Vec::new()],
            raster: ContourRaster::new(c(-1.0, -1.0), 1.0, 30, 30),
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            close_search_cones: Vec::new(),
            grown_by_growing_process: vec![false],
            growing_push_pull_vectors: Vec::new(),
            growing_integration_step_dots: Vec::new(),
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
    fn step3_rain_svg_has_only_rain_drop_points() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step3_rain.svg");
        let result = sample_result();
        let step3 = Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: vec![vec![c(1.0, 0.0)]],
            rain_hysteresis_points: Vec::new(),
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };
        write_step3_rain_svg(&path, &result, &step3).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count(&text, "<circle"), 3); // only the 3 rain-drop dots, no hysteresis markers
        assert_eq!(count(&text, "rgb(220,20,60)"), 1); // the raw contour, not an anti-rain-drop dot
    }

    #[test]
    fn step3_rain_svg_omits_a_gravity_arrow_for_a_contour_only_anti_rain_resolved() {
        // `result.contours` is shared, mutated-in-place state: by the time
        // any --create_svg file is written it already holds gravity from
        // every step that ran, including Anti Rain Drop Production. Without
        // `defined_after_rain` filtering the rain SVG's own gravity-arrow
        // layer, a contour no rain drop ever touched -- resolved only by
        // Anti Rain Drop Production, which by this point has already run --
        // would still show an arrow here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step3_rain.svg");

        let ls0 = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0), c(10.0, 0.0)]);
        let ls1 = LineString::new(vec![c(0.0, 5.0), c(5.0, 5.0), c(10.0, 5.0)]);
        let (gx0, gy0) = gravity_vector_for_side(&ls0, 1.0);
        let (gx1, gy1) = gravity_vector_for_side(&ls1, 1.0);
        let mut contour0 = Contour {
            lwg: LineWithGravity::new(ls0),
            elevation_height: None,
            empty_progeny: false,
        };
        contour0.lwg.gravity_dx = Some(gx0);
        contour0.lwg.gravity_dy = Some(gy0);
        let mut contour1 = Contour {
            lwg: LineWithGravity::new(ls1),
            elevation_height: None,
            empty_progeny: false,
        };
        contour1.lwg.gravity_dx = Some(gx1);
        contour1.lwg.gravity_dy = Some(gy1);

        let result = Step1Result {
            contours: vec![contour0, contour1],
            raw_polylines: vec![Vec::new(), Vec::new()],
            raster: ContourRaster::new(c(-1.0, -1.0), 1.0, 30, 30),
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            close_search_cones: Vec::new(),
            grown_by_growing_process: vec![false, false],
            growing_push_pull_vectors: Vec::new(),
            growing_integration_step_dots: Vec::new(),
            warnings: Vec::new(),
        };

        let step3 = Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
            defined_after_rain: vec![true, false],
            rain_paths: Vec::new(),
            anti_rain_paths: Vec::new(),
            rain_hysteresis_points: Vec::new(),
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };

        write_step3_rain_svg(&path, &result, &step3).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 3 nodes on each contour; only contour 0's arrows should be drawn.
        assert_eq!(count(&text, "rgb(230,200,20)"), 3);
    }

    #[test]
    fn step3_rain_svg_draws_a_gray_trail_under_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step3_rain.svg");
        let result = sample_result();
        let step3 = Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: Vec::new(),
            rain_hysteresis_points: vec![c(0.0, 0.0)],
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: vec![(c(0.0, 0.0), c(0.0, 1.0))],
            anti_rain_vote_segments: Vec::new(),
        };
        write_step3_rain_svg(&path, &result, &step3).unwrap();
        let full_text = std::fs::read_to_string(&path).unwrap();
        // `oob_area`'s own black polygon is always the very first layer,
        // drawn under everything else (see `base_layers`) -- excluded here
        // so its own "rgb(0,0,0)" fill doesn't get mistaken for the
        // hysteresis marker's.
        let oob_end = full_text
            .find(r#"fill-rule="evenodd""#)
            .expect("oob layer missing");
        let text = &full_text[oob_end..];

        // One gray trail path -- one rain path, drawn as a single polyline
        // through all 3 of its points, not per-step segments.
        let trail_width_attr = format!(r#"stroke-width="{DROP_TRAIL_STROKE_WIDTH}""#);
        assert_eq!(count(text, &trail_width_attr), 1);

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
    fn step3_anti_rain_svg_has_only_anti_rain_drop_points() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step3_anti_rain.svg");
        let result = sample_result();
        let step3 = Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: vec![vec![c(1.0, 0.0)]],
            rain_hysteresis_points: Vec::new(),
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };
        write_step3_anti_rain_svg(&path, &result, &step3).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count(&text, "<circle"), 1); // only the 1 anti-rain-drop dot, no hysteresis markers
    }

    #[test]
    fn step3_rain_svg_draws_a_black_marker_under_each_hysteresis_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step3_rain.svg");
        let result = sample_result();
        let step3 = Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: Vec::new(),
            // Whichever points step3_rain_drop::simulate_one_drop reported
            // as still inside a hysteresis window -- this layer just draws
            // them, it doesn't recompute which ones those are.
            rain_hysteresis_points: vec![c(0.0, 0.0), c(0.0, 1.0)],
            anti_rain_hysteresis_points: Vec::new(),
            rain_vote_segments: Vec::new(),
            anti_rain_vote_segments: Vec::new(),
        };
        write_step3_rain_svg(&path, &result, &step3).unwrap();
        let full_text = std::fs::read_to_string(&path).unwrap();
        // Excludes `oob_area`'s own black polygon, always the first layer
        // drawn (see `base_layers`) -- its own "rgb(0,0,0)" fill would
        // otherwise be mistaken for one of the hysteresis markers' below.
        let oob_end = full_text
            .find(r#"fill-rule="evenodd""#)
            .expect("oob layer missing");
        let text = &full_text[oob_end..];

        // 3 rain-drop dots, plus a black marker under each of the 2
        // hysteresis points.
        assert_eq!(count(text, "<circle"), 5);
        assert_eq!(count(text, r#"fill="rgb(0,0,0)""#), 2);

        let black_pos = text.find("rgb(0,0,0)").expect("hysteresis marker missing");
        let blue_pos = text.find("rgb(30,60,200)").expect("rain-drop dot missing");
        assert!(
            black_pos < blue_pos,
            "the hysteresis marker must be drawn before the rain-drop dot, so the \
             dot's own color still shows on top"
        );
    }

    #[test]
    fn step3_rain_svg_draws_a_purple_segment_for_each_vote_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step3_rain.svg");
        let result = sample_result();
        let step3 = Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
            defined_after_rain: vec![true],
            rain_paths: vec![vec![c(0.0, 0.0), c(0.0, 1.0), c(0.0, 2.0)]],
            anti_rain_paths: Vec::new(),
            rain_hysteresis_points: vec![c(0.0, 0.0)],
            anti_rain_hysteresis_points: Vec::new(),
            // Whichever step step3_rain_drop::simulate_one_drop reported as
            // where it actually cast a vote -- this layer just draws it, it
            // doesn't recompute which one that is.
            rain_vote_segments: vec![(c(0.0, 0.0), c(0.0, 1.0))],
            anti_rain_vote_segments: Vec::new(),
        };
        write_step3_rain_svg(&path, &result, &step3).unwrap();
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

    #[test]
    fn step4_svg_colors_a_resolved_contour_and_labels_its_height() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step4.svg");
        let mut result = sample_result();
        result.contours[0].elevation_height = Some(3.0);
        let step4 = Step4Result {
            resolved: 1,
            warnings: Vec::new(),
            tree_edges: Vec::new(),
            dead_ends: Vec::new(),
        };
        write_step4_svg(&path, &result, &step4).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(text.starts_with("<svg"));
        assert!(text.trim_end().ends_with("</svg>"));
        // The only resolved contour is also the only height on the map, so
        // the gradient falls back to its own midpoint color.
        assert_eq!(count(&text, r##"stroke="#800080""##), 1);
        assert_eq!(count(&text, "<text"), 1);
        assert!(
            text.contains(">0: 3</text>"),
            "expected the label to name the contour's own index (0) alongside its height (3): {text}"
        );
        assert_eq!(count(&text, "<rect"), 0, "no dead end was reported");
        assert_eq!(count(&text, "<line"), 0, "no tree edge was reported");
    }

    #[test]
    fn step4_svg_shows_an_undefined_contour_gray_and_marks_a_dead_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step4.svg");

        let ls0 = LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0)]);
        let ls1 = LineString::new(vec![c(0.0, 5.0), c(10.0, 5.0)]);
        let mut resolved = Contour {
            lwg: LineWithGravity::new(ls0.clone()),
            elevation_height: Some(0.0),
            empty_progeny: true,
        };
        resolved.lwg.gravity_dx = Some(0.0);
        resolved.lwg.gravity_dy = Some(1.0);
        let undefined = Contour {
            lwg: LineWithGravity::new(ls1.clone()),
            elevation_height: None,
            empty_progeny: false,
        };

        let mut raster = ContourRaster::new(c(-1.0, -1.0), 1.0, 15, 10);
        raster.write_contour(0, &ls0);
        raster.write_contour(1, &ls1);

        let result = Step1Result {
            contours: vec![resolved, undefined],
            raw_polylines: vec![Vec::new(), Vec::new()],
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            close_search_cones: Vec::new(),
            grown_by_growing_process: vec![false, false],
            growing_push_pull_vectors: Vec::new(),
            growing_integration_step_dots: Vec::new(),
            warnings: Vec::new(),
        };
        let step4 = Step4Result {
            resolved: 1,
            warnings: vec!["contour 1 (10.0m long) still has no elevation height".to_string()],
            tree_edges: Vec::new(),
            dead_ends: vec![0],
        };
        write_step4_svg(&path, &result, &step4).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert_eq!(
            count(&text, &format!(r#"stroke="{STEP4_UNDEFINED_COLOR}""#)),
            1,
            "the undefined contour should be drawn solid gray"
        );
        // Both contours get a label -- the resolved one names its own index
        // and height, the undefined one just its own bare index, so either
        // can still be found in the picture from a warning naming it by
        // index alone.
        assert_eq!(count(&text, "<text"), 2);
        assert!(text.contains(">0: 0</text>"), "resolved contour 0: {text}");
        assert!(text.contains(">1</text>"), "undefined contour 1, bare index: {text}");
        assert_eq!(count(&text, "<rect"), 1, "one dead end marker");
    }

    #[test]
    fn lerp_color_returns_each_endpoint_at_t_zero_and_one() {
        assert_eq!(lerp_color(BLUE, RED, 0.0), BLUE);
        assert_eq!(lerp_color(BLUE, RED, 1.0), RED);
    }

    #[test]
    fn lerp_color_is_the_channel_wise_midpoint_at_t_half() {
        let Color::Rgb(mr, mg, mb) = lerp_color(BLUE, RED, 0.5) else {
            panic!("expected Color::Rgb");
        };
        let Color::Rgb(br, bg, bb) = BLUE else { unreachable!() };
        let Color::Rgb(rr, rg, rb) = RED else { unreachable!() };
        assert_eq!(mr, ((br as i32 + rr as i32) / 2) as u8);
        assert_eq!(mg, ((bg as i32 + rg as i32) / 2) as u8);
        assert_eq!(mb, ((bb as i32 + rb as i32) / 2) as u8);
    }

    #[test]
    fn lerp_color_clamps_t_outside_zero_one() {
        assert_eq!(lerp_color(BLUE, RED, -5.0), BLUE);
        assert_eq!(lerp_color(BLUE, RED, 5.0), RED);
    }

    #[test]
    fn gravity_direction_segments_sorts_a_weak_and_a_strong_pixel_into_different_buckets() {
        let raster = ContourRaster::new(c(-5.0, -5.0), 1.0, 20, 20);

        // Two synthetic grid entries: pixel (2, 2) confidently decided
        // (magnitude 1.0), pixel (3, 3) barely above the drawing threshold
        // (magnitude just over `GRAVITY_DIRECTION_MIN_MAGNITUDE`) -- built
        // directly rather than through `step5_gravity_raster::resolve`,
        // since only the bucketing/coloring here is under test.
        let mut grid = vec![vec![None; 20]; 20];
        grid[2][2] = Some((1.0, 0.0));
        grid[3][3] = Some((GRAVITY_DIRECTION_MIN_MAGNITUDE + 0.01, 0.0));
        let gravity = crate::step5_gravity_raster::GravityRaster::for_test(grid, 20, 20);

        let buckets = gravity_direction_segments(&raster, &gravity);
        assert_eq!(buckets.len(), GRAVITY_INTENSITY_BUCKETS);

        let non_empty: Vec<usize> = buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.0.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(non_empty.len(), 2, "expected exactly two non-empty buckets: {non_empty:?}");
        assert_eq!(
            *non_empty.last().unwrap(),
            GRAVITY_INTENSITY_BUCKETS - 1,
            "the magnitude-1.0 pixel must land in the strongest (reddest) bucket"
        );
        assert!(
            non_empty[0] < GRAVITY_INTENSITY_BUCKETS - 1,
            "the barely-above-threshold pixel must land in a weaker (bluer) bucket than the \
             magnitude-1.0 one: {non_empty:?}"
        );
    }
}
