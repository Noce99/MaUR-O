//! Step 0 of `Contours-to-Raster.md`: pulling Contours, Slope Lines, Jumps
//! and Heavy Objects out of a parsed map, building the Contour Raster, and
//! turning the supporting symbols into gravity evidence.

use geo::{Coord, LineString, Polygon};

use crate::contour_geometry::{self, coords_to_linestrings, nearest_index};
use crate::contour_raster::ContourRaster;
use crate::contour_symbols::{classify_symbol, jump_gravity_side, SymbolFamily};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{
    gravity_vector_for_side, Contour, LineGravityDefiners, LineWithGravity, PointGravityDefiners,
};
use crate::map::{Map, ObjectKind, Symbol};

/// Everything needed to draw a diagnostic picture of a Contour Raster
/// conflict `extract` could not resolve by unifying the two contours
/// involved -- carried alongside the plain error message (see
/// [`ExtractError::Conflict`]) so `--create_svg` can still show exactly what
/// collided, and why it was not just a small digitizing gap, even though the
/// run itself must still fail.
pub struct ConflictDiagnostics {
    /// The Contour Raster as it stood at the moment of the conflict -- every
    /// contour successfully written so far, including `existing_contour_idx`.
    pub raster: ContourRaster,
    /// Every contour successfully added before the conflicting one was
    /// reached, parallel to what `raster` itself already reflects.
    pub contours_so_far: Vec<Contour>,
    /// The contour that was being read when it collided with
    /// `existing_contour_idx` -- never got an index or a place in
    /// `contours_so_far`, since the run failed before either could happen.
    pub new_ls: LineString<f64>,
    /// Which of `contours_so_far` the new one collided with.
    pub existing_contour_idx: u64,
    /// The world-space center of every conflicting pixel found between the
    /// two -- not just one, since that is what told `extract` this was a
    /// genuine, sustained overlap rather than a small splice-able gap.
    pub conflict_positions: Vec<Coord<f64>>,
}

/// `extract`'s own error: either a plain message, or -- for a Contour Raster
/// conflict it could not resolve -- a message plus enough state to draw a
/// diagnostic picture of it (see [`ConflictDiagnostics`]).
pub enum ExtractError {
    /// Any other, non-diagnosable failure.
    Message(String),
    /// An unresolved Contour Raster conflict.
    Conflict {
        /// The error's own human-readable message.
        message: String,
        /// Enough state to draw a picture of exactly what collided.
        diagnostics: Box<ConflictDiagnostics>,
    },
}

impl ExtractError {
    /// The error's own human-readable message, regardless of variant.
    pub fn message(&self) -> &str {
        match self {
            ExtractError::Message(m) => m,
            ExtractError::Conflict { message, .. } => message,
        }
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

// A hand-written, message-only Debug: `ConflictDiagnostics` holds a
// `ContourRaster`/`Vec<Contour>` that derive no `Debug` of their own (there
// is no useful textual form for a pixel grid), so deriving here would just
// push that requirement onto them for no real benefit -- the message alone
// is all `Result::unwrap`'s panic output needs.
impl std::fmt::Debug for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl From<String> for ExtractError {
    fn from(message: String) -> Self {
        ExtractError::Message(message)
    }
}

/// Everything Step 0 produces: the contours (gravity still mostly
/// undefined), the Contour Raster they were written to, the gravity evidence
/// Step 1 and Step 2 consume, and any warnings along the way (a Slope Line
/// with no contour under it, a Jump with no derivable direction, a
/// degenerate circle fit).
pub struct Step0Result {
    /// One entry per contour subpath found on the map.
    pub contours: Vec<Contour>,
    /// The same contours' raw, unprocessed node sequence (still curved,
    /// mm-on-paper converted straight to ground meters, no flattening or
    /// resampling), parallel to `contours` -- kept only for the
    /// `--create_svg` visualization's "before" picture.
    pub raw_polylines: Vec<Vec<contour_geometry::RawVertex>>,
    /// The rasterized contour set, at `config.rasterization_px_size`.
    pub raster: ContourRaster,
    /// Slope Line and Heavy-Object-intersection evidence.
    pub point_definers: Vec<PointGravityDefiners>,
    /// Jump evidence.
    pub line_definers: Vec<LineGravityDefiners>,
    /// Every Slope Line found, whether or not it found a contour within
    /// `slope_lines_contours_search_radius` (unlike `point_definers`, which
    /// only holds the ones that did) -- kept only for the `--create_svg`
    /// visualization's search-radius ring and slope-line-symbol layers, so a
    /// skipped Slope Line's own search area and symbol can still be
    /// inspected.
    pub slope_lines: Vec<SlopeLineMark>,
    /// The `slope_lines_contours_search_radius` this run used, in ground
    /// meters -- kept alongside `slope_lines` so the `--create_svg` layer
    /// can draw the ring at the right size without needing the whole
    /// `Config` passed through.
    pub slope_lines_contours_search_radius: f64,
    /// Every Heavy Object's own buffered polygon (Appendix 2, buffered by
    /// `heavy_object_width`/`heavy_object_growing` -- the same two
    /// parameters a Jump's own polygon uses), one per Heavy Object subpath,
    /// regardless of whether it found any contour inside it -- kept only for
    /// the `--create_svg` visualization, the same way a Jump's polygon is
    /// drawn (see `LineGravityDefiners::poly`), so the actual search area a
    /// `heavy_object_width`/`heavy_object_growing` choice produces can be
    /// judged by eye against the real pixels.
    pub heavy_object_polygons: Vec<Polygon<f64>>,
    /// Recoverable problems found along the way.
    pub warnings: Vec<String>,
}

/// One Slope Line found on the map: its own position and rotation, kept
/// (regardless of whether it resolved) for `--create_svg`'s benefit -- see
/// `Step0Result::slope_lines`.
#[derive(Clone, Copy)]
pub struct SlopeLineMark {
    /// Ground-meter position of the Slope Line symbol.
    pub pos: Coord<f64>,
    /// The .omap object's own rotation, radians. Gravity direction is
    /// `(-rotation.sin(), -rotation.cos())` (renderer.rs's rotatable-point
    /// placement -- see the comment where this is read in `extract`).
    pub rotation: f64,
    /// Whether this one found a contour within
    /// `slope_lines_contours_search_radius` and was pushed to
    /// `Step0Result::point_definers` -- kept so `--create_svg` can tell the
    /// two apart (e.g. by color) without re-deriving it.
    pub resolved: bool,
}

/// One classified, geometry-converted object, kept around between the
/// bounding-box pass and the raster-writing pass so the (potentially
/// expensive, curve-flattening) conversion only happens once per object.
enum Classified {
    Contour {
        linestrings: Vec<LineString<f64>>,
        raw: Vec<Vec<contour_geometry::RawVertex>>,
    },
    SlopeLine {
        pos: Coord<f64>,
        rotation: f64,
    },
    Jump {
        linestrings: Vec<LineString<f64>>,
        side: Option<f64>,
    },
    HeavyObject(Vec<LineString<f64>>),
}

fn extend_bbox(min: &mut Coord<f64>, max: &mut Coord<f64>, p: Coord<f64>) {
    min.x = min.x.min(p.x);
    min.y = min.y.min(p.y);
    max.x = max.x.max(p.x);
    max.y = max.y.max(p.y);
}

fn extend_bbox_ls(min: &mut Coord<f64>, max: &mut Coord<f64>, ls: &LineString<f64>) {
    for &p in &ls.0 {
        extend_bbox(min, max, p);
    }
}

/// Classifies and geometry-converts every object on the map, in ground
/// meters. Objects with an unresolved or unclassified symbol are skipped
/// silently -- being neither Contour, Slope Line, Jump nor Heavy Object is
/// the ordinary case for most of a map's objects.
fn classify_all(map: &Map, config: &Config, meters_per_mm: f64) -> Vec<Classified> {
    let mut out = Vec::new();

    for coords in merge_contour_object_chains(map) {
        let linestrings = coords_to_linestrings(
            &coords,
            meters_per_mm,
            config.bezier_linearization_step,
            config.contours_step,
        );
        let raw = contour_geometry::raw_polylines(&coords, meters_per_mm);
        out.push(Classified::Contour { linestrings, raw });
    }

    for object in &map.objects {
        let Some(symbol_index) = object.symbol_index else {
            continue;
        };
        let symbol = &map.symbols[symbol_index];
        let Some(family) = classify_symbol(symbol) else {
            continue;
        };
        match family {
            SymbolFamily::Contour => {} // handled above, by merge_contour_object_chains
            SymbolFamily::SlopeLine => {
                let ObjectKind::Point = object.kind else {
                    continue;
                };
                let Some(first) = object.coords.first() else {
                    continue;
                };
                out.push(Classified::SlopeLine {
                    pos: Coord {
                        x: first.x * meters_per_mm,
                        y: first.y * meters_per_mm,
                    },
                    rotation: object.rotation,
                });
            }
            SymbolFamily::Jump => {
                let Symbol::Line(line_symbol) = symbol else {
                    continue;
                };
                let side = jump_gravity_side(line_symbol);
                let lss = coords_to_linestrings(
                    &object.coords,
                    meters_per_mm,
                    config.bezier_linearization_step,
                    config.contours_step,
                );
                out.push(Classified::Jump {
                    linestrings: lss,
                    side,
                });
            }
            SymbolFamily::HeavyObject => {
                let lss = coords_to_linestrings(
                    &object.coords,
                    meters_per_mm,
                    config.bezier_linearization_step,
                    config.contours_step,
                );
                out.push(Classified::HeavyObject(lss));
            }
        }
    }
    out
}

/// A coordinate's position, rounded to a hashable key: real map data (see
/// below) repeats a shared endpoint's coordinates bit-for-bit, but a small
/// rounding tolerance is cheap insurance against float noise doing the same
/// join by hand would not have.
fn endpoint_key(p: crate::map::Point) -> (i64, i64) {
    ((p.x * 1e6).round() as i64, (p.y * 1e6).round() as i64)
}

/// Chains Contour-family objects whose raw endpoints coincide into one
/// continuous [`CoordList`] each.
///
/// Mapper sometimes splits one physical contour line across several `.omap`
/// objects -- observed directly in real map data (`maps/forest_sample.omap`):
/// one Contour object's last coordinate is bit-identical to the next
/// Contour object's first. Left unmerged, Step 0 would treat the two pieces
/// as separate `Contour`s that happen to touch at exactly one point, which
/// both crashes the Contour Raster's conflict check (both pieces claim the
/// pixel at the shared point) and would wrongly split one contour's gravity
/// evidence -- votes, Slope Lines, circle-fit readings -- across two
/// `Contour`s that should be one, risking each resolving to a different
/// (and, per Assumption 1, contradictory) gravity direction.
fn merge_contour_object_chains(map: &Map) -> Vec<crate::map::CoordList> {
    struct Piece {
        object_index: usize,
        start: crate::map::Point,
        end: crate::map::Point,
    }

    let mut pieces = Vec::new();
    for (object_index, object) in map.objects.iter().enumerate() {
        let Some(symbol_index) = object.symbol_index else {
            continue;
        };
        if classify_symbol(&map.symbols[symbol_index]) != Some(SymbolFamily::Contour) {
            continue;
        }
        if object.coords.len() < 2 {
            continue;
        }
        pieces.push(Piece {
            object_index,
            start: object.coords.first().unwrap().pos(),
            end: object.coords.last().unwrap().pos(),
        });
    }

    let mut by_start: std::collections::HashMap<(i64, i64), Vec<usize>> =
        std::collections::HashMap::new();
    for (i, piece) in pieces.iter().enumerate() {
        by_start
            .entry(endpoint_key(piece.start))
            .or_default()
            .push(i);
    }
    // A piece claimed as another's continuation isn't a chain start of its own.
    let mut is_continuation = vec![false; pieces.len()];
    let mut next: Vec<Option<usize>> = vec![None; pieces.len()];
    for (i, piece) in pieces.iter().enumerate() {
        if let Some(candidates) = by_start.get(&endpoint_key(piece.end)) {
            if let Some(&n) = candidates.iter().find(|&&c| c != i && !is_continuation[c]) {
                next[i] = Some(n);
                is_continuation[n] = true;
            }
        }
    }

    let mut visited = vec![false; pieces.len()];
    let mut chains: Vec<Vec<usize>> = Vec::new();
    // Chains with a distinct start first, so a cycle (a closed contour whose
    // pieces loop back on themselves) is only picked up afterwards, from
    // whichever of its pieces is left unvisited.
    for start in (0..pieces.len()).filter(|&i| !is_continuation[i]) {
        let mut chain = vec![start];
        visited[start] = true;
        let mut cur = start;
        while let Some(n) = next[cur] {
            if visited[n] {
                break;
            }
            chain.push(n);
            visited[n] = true;
            cur = n;
        }
        chains.push(chain);
    }
    let still_unvisited: Vec<usize> = (0..pieces.len()).filter(|&i| !visited[i]).collect();
    for start in still_unvisited {
        if visited[start] {
            continue; // may have been swept into an earlier cycle already
        }
        let mut chain = vec![start];
        visited[start] = true;
        let mut cur = start;
        while let Some(n) = next[cur] {
            if visited[n] {
                break;
            }
            chain.push(n);
            visited[n] = true;
            cur = n;
        }
        chains.push(chain);
    }

    chains
        .into_iter()
        .map(|chain| {
            let mut coords = map.objects[pieces[chain[0]].object_index].coords.clone();
            for &piece_index in &chain[1..] {
                coords.pop(); // the previous piece's last coordinate duplicates this one's first
                coords.extend(
                    map.objects[pieces[piece_index].object_index]
                        .coords
                        .iter()
                        .copied(),
                );
            }
            coords
        })
        .collect()
}

/// Kasa algebraic least-squares circle fit: the center and radius of the
/// circle that best fits `points` in a least-squares sense, or `None` if
/// there are too few points, or they are too close to collinear for a
/// numerically stable fit (a zero or negative fitted radius-squared is what
/// collinear points produce). Points are centered on their own centroid
/// before solving, for conditioning -- Heavy Object intersections sit at
/// whatever a map's own ground coordinates are, and the linear system is
/// only well conditioned close to its own origin.
fn kasa_circle_fit(points: &[Coord<f64>]) -> Option<(Coord<f64>, f64)> {
    let n = points.len();
    if n < 3 {
        return None;
    }
    let n_f = n as f64;
    let (mut cx, mut cy) = (0.0, 0.0);
    for p in points {
        cx += p.x;
        cy += p.y;
    }
    cx /= n_f;
    cy /= n_f;

    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    let (mut sx, mut sy) = (0.0, 0.0);
    let (mut sxz, mut syz, mut sz) = (0.0, 0.0, 0.0);
    for p in points {
        let (x, y) = (p.x - cx, p.y - cy);
        let z = x * x + y * y;
        sxx += x * x;
        sxy += x * y;
        syy += y * y;
        sx += x;
        sy += y;
        sxz += x * z;
        syz += y * z;
        sz += z;
    }

    // Solve for (d, e, f) minimizing sum (z_i + d*x_i + e*y_i + f)^2, i.e.
    // the circle x^2 + y^2 + d*x + e*y + f = 0 in centered coordinates.
    let m = [[sxx, sxy, sx], [sxy, syy, sy], [sx, sy, n_f]];
    let b = [-sxz, -syz, -sz];
    let [d, e, f] = solve3(m, b)?;

    let center = Coord {
        x: cx - d / 2.0,
        y: cy - e / 2.0,
    };
    let r2 = (d * d + e * e) / 4.0 - f;
    if r2 <= 0.0 {
        return None;
    }
    Some((center, r2.sqrt()))
}

/// Solves the 3x3 linear system `m * x = b` by Cramer's rule, or `None` if
/// `m` is (numerically) singular.
fn solve3(m: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    fn det3(m: &[[f64; 3]; 3]) -> f64 {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }
    let det = det3(&m);
    if det.abs() < 1e-9 {
        return None;
    }
    let mut mx = m;
    mx[0][0] = b[0];
    mx[1][0] = b[1];
    mx[2][0] = b[2];
    let mut my = m;
    my[0][1] = b[0];
    my[1][1] = b[1];
    my[2][1] = b[2];
    let mut mz = m;
    mz[0][2] = b[0];
    mz[1][2] = b[1];
    mz[2][2] = b[2];
    Some([det3(&mx) / det, det3(&my) / det, det3(&mz) / det])
}

/// Whether `ls`'s own start node lies within `radius` ground meters of
/// `near` -- `Some(true)`, or its end node does -- `Some(false)` (whichever
/// is closer, if both are), or `None` if neither is. Used to tell which end
/// of a contour a Contour Raster conflict happened next to, so the two
/// contours it involves can be joined at the right ends.
fn nearest_end_is_start(ls: &LineString<f64>, near: Coord<f64>, radius: f64) -> Option<bool> {
    let start = *ls.0.first()?;
    let end = *ls.0.last()?;
    let dist = |p: Coord<f64>| (p.x - near.x).hypot(p.y - near.y);
    let (d_start, d_end) = (dist(start), dist(end));
    if d_start > radius && d_end > radius {
        return None;
    }
    Some(d_start <= d_end)
}

/// Joins `a` and `b` into one continuous `LineString`, oriented so the ends
/// nearest `near` actually meet, if (and only if) both of them have an end
/// within `radius` ground meters of `near` -- otherwise `None`.
///
/// Real contour digitizing sometimes splits one physical line into two
/// objects whose endpoints are close but not bit-identical (unlike
/// `merge_contour_object_chains`'s exact-match join, done earlier on the raw
/// `.omap` coordinates, before any geometry conversion); this is Step 0's
/// later, geometry-based fallback for that same situation once it surfaces
/// as a Contour Raster pixel conflict. Neither contour has any gravity of
/// its own yet at this point in Step 0, so reordering/reversing either
/// one's nodes here is free of any of the direction-dependent meaning
/// `LineWithGravity`'s own fields carry once Step 1/2 run.
fn merge_close_endpoints(
    a: &LineString<f64>,
    b: &LineString<f64>,
    near: Coord<f64>,
    radius: f64,
) -> Option<LineString<f64>> {
    let a_is_start = nearest_end_is_start(a, near, radius)?;
    let b_is_start = nearest_end_is_start(b, near, radius)?;
    // Both halves end up with their own near end last/first respectively,
    // so simply concatenating them joins the two close ends together.
    let mut merged = a.0.clone();
    if a_is_start {
        merged.reverse();
    }
    let mut tail = b.0.clone();
    if !b_is_start {
        tail.reverse();
    }
    merged.extend(tail);
    Some(LineString::new(merged))
}

/// The number of *distinct* points in a `LineString`, treating a closed
/// one's repeated last point as the same point as its first.
fn ring_len(ls: &LineString<f64>) -> usize {
    if ls.is_closed() && ls.0.len() > 1 {
        ls.0.len() - 1
    } else {
        ls.0.len()
    }
}

/// The point `offset` steps away from `center` along `ls`, wrapping around a
/// closed contour or `None` past either end of an open one.
fn point_offset(ls: &LineString<f64>, center: usize, offset: isize) -> Option<Coord<f64>> {
    let n = ring_len(ls);
    if n == 0 {
        return None;
    }
    if ls.is_closed() && ls.0.len() > 1 {
        let idx = (center as isize + offset).rem_euclid(n as isize) as usize;
        Some(ls.0[idx])
    } else {
        let idx = center as isize + offset;
        if idx < 0 || idx as usize >= ls.0.len() {
            None
        } else {
            Some(ls.0[idx as usize])
        }
    }
}

/// Fits a circle to `contour_idx`'s own `ls` around the point nearest `at`
/// (using `circumference_fitting_points_number` points on each side), then
/// either pushes a `PointGravityDefiners` reading at `at` with gravity
/// pointing from `at` toward the fitted circle's center, or -- on a
/// degenerate fit, or one centered on `at` itself -- a warning instead.
#[allow(clippy::too_many_arguments)]
fn push_heavy_object_reading(
    contours: &[Contour],
    contour_idx: u64,
    at: Coord<f64>,
    circumference_fitting_points_number: usize,
    point_definers: &mut Vec<PointGravityDefiners>,
    warnings: &mut Vec<String>,
) {
    let contour_ls = &contours[contour_idx as usize].lwg.ls;
    let center_idx = nearest_index(contour_ls, at);
    let mut fit_points = Vec::new();
    for k in 1..=circumference_fitting_points_number as isize {
        if let Some(p) = point_offset(contour_ls, center_idx, -k) {
            fit_points.push(p);
        }
        if let Some(p) = point_offset(contour_ls, center_idx, k) {
            fit_points.push(p);
        }
    }
    match kasa_circle_fit(&fit_points) {
        Some((center, _radius)) => {
            let (dx, dy) = (center.x - at.x, center.y - at.y);
            let len = dx.hypot(dy);
            if len < 1e-6 {
                warnings.push(format!(
                    "Heavy Object intersection with contour {contour_idx} at \
                     ({:.2}, {:.2}) fit a circle centered on the intersection \
                     itself; skipped",
                    at.x, at.y
                ));
            } else {
                point_definers.push(PointGravityDefiners {
                    x: at.x,
                    y: at.y,
                    reference_contour: contour_idx,
                    gravity_dx: Some(dx / len),
                    gravity_dy: Some(dy / len),
                });
            }
        }
        None => {
            warnings.push(format!(
                "could not fit a circle to contour {contour_idx} near a Heavy \
                 Object intersection at ({:.2}, {:.2}) (near-collinear points); \
                 skipped",
                at.x, at.y
            ));
        }
    }
}

/// Runs Step 0: extracts Contours, Slope Lines, Jumps and Heavy Objects from
/// `map`, builds the Contour Raster, and turns the supporting symbols into
/// gravity evidence for Step 1 and Step 2.
pub fn extract(map: &Map, config: &Config) -> Result<Step0Result, ExtractError> {
    let meters_per_mm = contour_geometry::meters_per_mm(map.scale_denominator);
    let mut warnings = Vec::new();

    let classified = classify_all(map, config, meters_per_mm);

    let mut min = Coord {
        x: f64::INFINITY,
        y: f64::INFINITY,
    };
    let mut max = Coord {
        x: f64::NEG_INFINITY,
        y: f64::NEG_INFINITY,
    };
    for c in &classified {
        match c {
            Classified::Contour {
                linestrings: lss, ..
            }
            | Classified::HeavyObject(lss) => {
                for ls in lss {
                    extend_bbox_ls(&mut min, &mut max, ls);
                }
            }
            Classified::Jump { linestrings, .. } => {
                for ls in linestrings {
                    extend_bbox_ls(&mut min, &mut max, ls);
                }
            }
            Classified::SlopeLine { pos, .. } => extend_bbox(&mut min, &mut max, *pos),
        }
    }
    if !min.x.is_finite() {
        return Err(ExtractError::Message(
            "no Contour, Slope Line, Jump or Heavy Object symbols were found on the map"
                .to_string(),
        ));
    }

    let pad = config.heavy_object_width
        + config.heavy_object_growing
        + 5.0 * config.rasterization_px_size;
    min.x -= pad;
    min.y -= pad;
    max.x += pad;
    max.y += pad;

    let width = ((max.x - min.x) / config.rasterization_px_size)
        .ceil()
        .max(1.0) as usize;
    let height = ((max.y - min.y) / config.rasterization_px_size)
        .ceil()
        .max(1.0) as usize;
    let mut raster = ContourRaster::new(min, config.rasterization_px_size, width, height);

    let mut contours: Vec<Contour> = Vec::new();
    let mut raw_polylines: Vec<Vec<contour_geometry::RawVertex>> = Vec::new();
    for c in &classified {
        if let Classified::Contour {
            linestrings: lss,
            raw,
        } = c
        {
            for (ls, raw_poly) in lss.iter().zip(raw) {
                let densified = contour_geometry::densify(
                    ls,
                    config.rasterization_step_factor,
                    config.rasterization_px_size,
                );
                let idx = contours.len() as u64;
                let conflicts = raster.find_conflicts(idx, &densified);
                if conflicts.is_empty() {
                    raster.write_contour(idx, &densified)?;
                    contours.push(Contour {
                        lwg: LineWithGravity::new(ls.clone()),
                        elevation_height: None,
                    });
                    raw_polylines.push(raw_poly.clone());
                    continue;
                }

                // Every conflicting pixel must point at the same already-
                // written contour, and every one of them (not just one) must
                // fall near both contours' own terminal nodes -- otherwise
                // this is a genuine, sustained overlap (e.g. two truly
                // parallel, too-closely-spaced contours), not a small
                // digitizing gap, and must still crash.
                let existing_idx = conflicts[0].1;
                let existing_ls = &contours[existing_idx as usize].lwg.ls;
                let mergeable = conflicts.iter().all(|&(pos, i)| {
                    i == existing_idx
                        && !existing_ls.is_closed()
                        && !ls.is_closed()
                        && nearest_end_is_start(existing_ls, pos, config.contour_gap_merge_radius)
                            .is_some()
                        && nearest_end_is_start(ls, pos, config.contour_gap_merge_radius).is_some()
                });
                let merged_ls = mergeable
                    .then(|| {
                        merge_close_endpoints(
                            existing_ls,
                            ls,
                            conflicts[0].0,
                            config.contour_gap_merge_radius,
                        )
                    })
                    .flatten();
                let Some(merged_ls) = merged_ls else {
                    let message = format!(
                        "the Contour Raster pixel at ({:.2}, {:.2}) is claimed by two \
                         different contours (index {existing_idx} and a newly read one); \
                         decrease rasterization_px_size, or -- if these are really the same \
                         physical line split by a small digitizing gap -- increase \
                         contour_gap_merge_radius",
                        conflicts[0].0.x, conflicts[0].0.y
                    );
                    return Err(ExtractError::Conflict {
                        message,
                        diagnostics: Box::new(ConflictDiagnostics {
                            raster,
                            contours_so_far: contours,
                            new_ls: ls.clone(),
                            existing_contour_idx: existing_idx,
                            conflict_positions: conflicts.iter().map(|&(pos, _)| pos).collect(),
                        }),
                    });
                };
                let merged_densified = contour_geometry::densify(
                    &merged_ls,
                    config.rasterization_step_factor,
                    config.rasterization_px_size,
                );
                raster.write_contour(existing_idx, &merged_densified)?;
                contours[existing_idx as usize].lwg = LineWithGravity::new(merged_ls);
                // Only cosmetic (the `--create_svg` "before" picture): not
                // reordered to match the merge's own end-matching, since a
                // RawVertex's is_curve_start marks the first of a run of
                // four vertices forming one Bezier segment, and reversing
                // that grouping correctly is more machinery than a
                // visualization-only picture is worth.
                raw_polylines[existing_idx as usize].extend(raw_poly.iter().copied());
                warnings.push(format!(
                    "two contour objects near ({:.2}, {:.2}) had endpoints closer than \
                     contour_gap_merge_radius ({}m) apart; joined into one contour \
                     (index {existing_idx}) instead of crashing on the Contour Raster conflict",
                    conflicts[0].0.x, conflicts[0].0.y, config.contour_gap_merge_radius
                ));
            }
        }
    }
    if contours.is_empty() {
        return Err(ExtractError::Message(
            "no Contour symbols (codes 101/102) were found on the map".to_string(),
        ));
    }

    let mut point_definers = Vec::new();
    let mut line_definers = Vec::new();
    let mut slope_lines = Vec::new();
    let mut heavy_object_polygons = Vec::new();

    for c in &classified {
        match c {
            Classified::SlopeLine { pos, rotation } => {
                let reference_contour = raster
                    .nearest_contour_within_radius(*pos, config.slope_lines_contours_search_radius);
                slope_lines.push(SlopeLineMark {
                    pos: *pos,
                    rotation: *rotation,
                    resolved: reference_contour.is_some(),
                });
                let Some(reference_contour) = reference_contour else {
                    warnings.push(format!(
                        "slope line at ({:.2}, {:.2}) has no contour within \
                         slope_lines_contours_search_radius ({}m); skipped",
                        pos.x, pos.y, config.slope_lines_contours_search_radius
                    ));
                    continue;
                };
                // Local point "up" (0,-1), placed at rotation = -object.rotation
                // (renderer.rs's rotatable-point placement): global gravity
                // = (-sin(rotation), -cos(rotation)). See contour_symbols.rs.
                point_definers.push(PointGravityDefiners {
                    x: pos.x,
                    y: pos.y,
                    reference_contour,
                    gravity_dx: Some(-rotation.sin()),
                    gravity_dy: Some(-rotation.cos()),
                });
            }
            Classified::Jump { linestrings, side } => {
                let Some(side) = side else {
                    warnings.push(
                        "a Jump symbol has no rotatable directional tick to derive gravity from; skipped".to_string(),
                    );
                    continue;
                };
                for ls in linestrings {
                    if ls.0.len() < 2 {
                        continue;
                    }
                    let (gx, gy) = gravity_vector_for_side(ls, *side);
                    let mut lwg = LineWithGravity::new(ls.clone());
                    lwg.gravity_dx = Some(gx);
                    lwg.gravity_dy = Some(gy);
                    let poly = contour_geometry::ls_to_polygon(
                        ls,
                        config.heavy_object_width,
                        config.heavy_object_growing,
                    );
                    line_definers.push(LineGravityDefiners { lwg, poly });
                }
            }
            Classified::HeavyObject(lss) => {
                for ls in lss {
                    // Buffered the same way a Jump's own ls is (Appendix 2,
                    // heavy_object_width/heavy_object_growing) -- searching
                    // every pixel inside this polygon, not just the ones
                    // directly under the digitized line, catches a contour
                    // that runs close to a Heavy Object without landing
                    // exactly on it, the same motivation as
                    // slope_lines_contours_search_radius for Slope Lines.
                    let poly = contour_geometry::ls_to_polygon(
                        ls,
                        config.heavy_object_width,
                        config.heavy_object_growing,
                    );
                    // Group every non-zero pixel the polygon covers by which
                    // contour it belongs to -- a BTreeMap, not a HashMap, so
                    // the order these readings are produced in stays the
                    // same across runs. One reading per contour the polygon
                    // touches, at that contour's own matched pixels'
                    // centroid, rather than one per pixel: the polygon
                    // commonly covers many pixels of the very same nearby
                    // contour (its own width), and treating each separately
                    // would flood point_definers with near-duplicate circle
                    // fits of the same physical crossing.
                    let mut hits: std::collections::BTreeMap<u64, (Coord<f64>, usize)> =
                        std::collections::BTreeMap::new();
                    for (px, py) in raster.pixels_in_polygon(&poly) {
                        let val = raster.get(px, py);
                        if val == 0 {
                            continue;
                        }
                        let contour_idx = (val - 1) as u64;
                        let center = raster.pixel_center(px, py);
                        let entry = hits
                            .entry(contour_idx)
                            .or_insert((Coord { x: 0.0, y: 0.0 }, 0));
                        entry.0.x += center.x;
                        entry.0.y += center.y;
                        entry.1 += 1;
                    }
                    for (contour_idx, (sum, count)) in hits {
                        let at = Coord {
                            x: sum.x / count as f64,
                            y: sum.y / count as f64,
                        };
                        push_heavy_object_reading(
                            &contours,
                            contour_idx,
                            at,
                            config.circumference_fitting_points_number,
                            &mut point_definers,
                            &mut warnings,
                        );
                    }
                    heavy_object_polygons.push(poly);
                }
            }
            Classified::Contour { .. } => {}
        }
    }

    Ok(Step0Result {
        contours,
        raw_polylines,
        raster,
        point_definers,
        line_definers,
        slope_lines,
        slope_lines_contours_search_radius: config.slope_lines_contours_search_radius,
        heavy_object_polygons,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    #[test]
    fn kasa_fit_recovers_a_known_circle() {
        let center = c(5.0, -3.0);
        let radius = 4.0;
        let points: Vec<Coord<f64>> = (0..12)
            .map(|i| {
                let a = (i as f64) * std::f64::consts::TAU / 12.0;
                c(center.x + radius * a.cos(), center.y + radius * a.sin())
            })
            .collect();
        let (fit_center, fit_radius) = kasa_circle_fit(&points).unwrap();
        assert!((fit_center.x - center.x).abs() < 1e-6);
        assert!((fit_center.y - center.y).abs() < 1e-6);
        assert!((fit_radius - radius).abs() < 1e-6);
    }

    #[test]
    fn kasa_fit_rejects_collinear_points() {
        let points: Vec<Coord<f64>> = (0..5).map(|i| c(i as f64, 2.0 * i as f64)).collect();
        assert!(kasa_circle_fit(&points).is_none());
    }

    #[test]
    fn kasa_fit_rejects_too_few_points() {
        assert!(kasa_circle_fit(&[c(0.0, 0.0), c(1.0, 0.0)]).is_none());
    }

    #[test]
    fn push_heavy_object_reading_points_toward_the_fitted_circles_center() {
        let center = c(5.0, -3.0);
        let radius = 4.0;
        let points: Vec<Coord<f64>> = (0..12)
            .map(|i| {
                let a = (i as f64) * std::f64::consts::TAU / 12.0;
                c(center.x + radius * a.cos(), center.y + radius * a.sin())
            })
            .collect();
        let mut ring = points.clone();
        ring.push(points[0]);
        let contours = vec![Contour {
            lwg: LineWithGravity::new(LineString::new(ring)),
            elevation_height: None,
        }];

        let at = points[0]; // a point actually on the circle
        let mut point_definers = Vec::new();
        let mut warnings = Vec::new();
        push_heavy_object_reading(&contours, 0, at, 4, &mut point_definers, &mut warnings);

        assert!(warnings.is_empty());
        assert_eq!(point_definers.len(), 1);
        let d = &point_definers[0];
        assert_eq!((d.x, d.y), (at.x, at.y));
        assert_eq!(d.reference_contour, 0);
        let (dx, dy) = (d.gravity_dx.unwrap(), d.gravity_dy.unwrap());
        let (want_dx, want_dy) = (center.x - at.x, center.y - at.y);
        let want_len = want_dx.hypot(want_dy);
        assert!((dx - want_dx / want_len).abs() < 1e-6);
        assert!((dy - want_dy / want_len).abs() < 1e-6);
    }

    #[test]
    fn push_heavy_object_reading_warns_on_collinear_points_instead_of_pushing() {
        let ls = LineString::new((0..10).map(|i| c(i as f64, 0.0)).collect::<Vec<_>>());
        let contours = vec![Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
        }];

        let mut point_definers = Vec::new();
        let mut warnings = Vec::new();
        push_heavy_object_reading(
            &contours,
            0,
            c(5.0, 0.0),
            4,
            &mut point_definers,
            &mut warnings,
        );

        assert!(point_definers.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("near-collinear"));
    }

    #[test]
    fn extract_finds_a_heavy_object_intersection_via_its_buffered_polygon_not_just_its_line() {
        use crate::map::{Coord as MapCoord, LineSymbol, Object, PathObject};

        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            rasterization_step_factor: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            contour_gap_merge_radius: 2.0,
        };

        let contour_symbol = Symbol::Line(LineSymbol {
            code: "101".to_string(),
            ..Default::default()
        });
        let heavy_symbol = Symbol::Line(LineSymbol {
            code: "107".to_string(),
            ..Default::default()
        });

        // A bent contour (a genuine corner at x=10, not a straight line) so
        // the circle fit around the crossing finds a real center instead of
        // rejecting collinear points.
        let contour_obj = Object {
            kind: ObjectKind::Path(PathObject::default()),
            symbol_id: 0,
            symbol_index: Some(0),
            coords: vec![
                MapCoord::new(0.0, 5.0, 0),
                MapCoord::new(10.0, 5.0, 0),
                MapCoord::new(20.0, 0.0, 0),
            ],
            rotation: 0.0,
        };
        // A Heavy Object crossing the contour at (10, 5), buffered wide
        // enough (heavy_object_width + heavy_object_growing = 1.2m, at a
        // 0.5m pixel size) that its polygon covers several pixels of the
        // same contour near the crossing.
        let heavy_obj = Object {
            kind: ObjectKind::Path(PathObject::default()),
            symbol_id: 1,
            symbol_index: Some(1),
            coords: vec![MapCoord::new(10.0, 0.0, 0), MapCoord::new(10.0, 10.0, 0)],
            rotation: 0.0,
        };

        let map = Map {
            scale_denominator: 1000, // meters_per_mm == 1.0
            colors: Vec::new(),
            symbols: vec![contour_symbol, heavy_symbol],
            symbol_ids: vec![0, 1],
            objects: vec![contour_obj, heavy_obj],
            georeferencing: None,
            symbol_set: None,
        };

        let result = extract(&map, &config).unwrap();

        assert_eq!(
            result.heavy_object_polygons.len(),
            1,
            "one polygon per Heavy Object, drawn regardless of what it intersects"
        );
        assert_eq!(
            result.point_definers.len(),
            1,
            "the buffered polygon covers several pixels of the same contour near the \
             crossing -- they must collapse into a single reading, not one per pixel"
        );
        assert_eq!(result.point_definers[0].reference_contour, 0);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn nearest_end_is_start_picks_whichever_end_is_closer() {
        let ls = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0), c(10.0, 0.0)]);
        assert_eq!(nearest_end_is_start(&ls, c(0.2, 0.0), 1.0), Some(true));
        assert_eq!(nearest_end_is_start(&ls, c(9.8, 0.0), 1.0), Some(false));
        assert_eq!(nearest_end_is_start(&ls, c(5.0, 0.0), 1.0), None);
    }

    #[test]
    fn merge_close_endpoints_joins_at_the_closer_ends_in_either_orientation() {
        let a = LineString::new(vec![c(0.0, 0.0), c(5.0, 0.0)]);
        let b = LineString::new(vec![c(5.05, 0.0), c(10.0, 0.0)]);
        // a's end meets b's start: straight concatenation.
        let merged = merge_close_endpoints(&a, &b, c(5.0, 0.0), 1.0).unwrap();
        assert_eq!(
            merged.0,
            vec![c(0.0, 0.0), c(5.0, 0.0), c(5.05, 0.0), c(10.0, 0.0)]
        );

        // a's end meets b's end (b digitized the other way around): b is
        // reversed before joining.
        let b_rev = LineString::new(vec![c(10.0, 0.0), c(5.05, 0.0)]);
        let merged = merge_close_endpoints(&a, &b_rev, c(5.0, 0.0), 1.0).unwrap();
        assert_eq!(
            merged.0,
            vec![c(0.0, 0.0), c(5.0, 0.0), c(5.05, 0.0), c(10.0, 0.0)]
        );
    }

    #[test]
    fn merge_close_endpoints_is_none_when_the_conflict_is_nowhere_near_either_end() {
        let a = LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0)]);
        let b = LineString::new(vec![c(0.0, 0.1), c(10.0, 0.1)]);
        // Two long parallel lines: the "conflict" sits in the middle of
        // both, nowhere near either one's own start or end.
        assert!(merge_close_endpoints(&a, &b, c(5.0, 0.05), 1.0).is_none());
    }

    #[test]
    fn extract_unifies_two_contours_split_by_a_small_digitizing_gap() {
        use crate::map::{Coord as MapCoord, LineSymbol, Object, PathObject};

        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            rasterization_step_factor: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            contour_gap_merge_radius: 1.0,
        };

        let contour_symbol = Symbol::Line(LineSymbol {
            code: "101".to_string(),
            ..Default::default()
        });

        // Two straight contour pieces, 0.05m apart at x=5 -- close enough
        // that a 0.5m Contour Raster pixel written by both collides, but far
        // too small a gap to be two genuinely distinct contours.
        let piece_a = Object {
            kind: ObjectKind::Path(PathObject::default()),
            symbol_id: 0,
            symbol_index: Some(0),
            coords: vec![MapCoord::new(0.0, 0.0, 0), MapCoord::new(5.0, 0.0, 0)],
            rotation: 0.0,
        };
        let piece_b = Object {
            kind: ObjectKind::Path(PathObject::default()),
            symbol_id: 0,
            symbol_index: Some(0),
            coords: vec![MapCoord::new(5.05, 0.0, 0), MapCoord::new(10.0, 0.0, 0)],
            rotation: 0.0,
        };

        let map = Map {
            scale_denominator: 1000, // meters_per_mm == 1.0
            colors: Vec::new(),
            symbols: vec![contour_symbol],
            symbol_ids: vec![0],
            objects: vec![piece_a, piece_b],
            georeferencing: None,
            symbol_set: None,
        };

        let result = extract(&map, &config).unwrap();

        assert_eq!(
            result.contours.len(),
            1,
            "the two pieces must be joined into a single contour, not crash or stay separate"
        );
        assert_eq!(
            result.warnings.len(),
            1,
            "joining them is a recoverable, warned-about condition, not a silent one"
        );
        assert!(result.warnings[0].contains("contour_gap_merge_radius"));
        let ls = &result.contours[0].lwg.ls;
        assert!((ls.0.first().unwrap().x - 0.0).abs() < 1e-6);
        assert!((ls.0.last().unwrap().x - 10.0).abs() < 1e-6);
    }

    #[test]
    fn point_offset_wraps_on_closed_contours() {
        let ls = LineString::new(vec![c(0.0, 0.0), c(1.0, 0.0), c(2.0, 0.0), c(0.0, 0.0)]);
        assert_eq!(point_offset(&ls, 0, -1), Some(c(2.0, 0.0)));
        assert_eq!(point_offset(&ls, 2, 1), Some(c(0.0, 0.0)));
    }

    #[test]
    fn point_offset_clamps_on_open_contours() {
        let ls = LineString::new(vec![c(0.0, 0.0), c(1.0, 0.0), c(2.0, 0.0)]);
        assert_eq!(point_offset(&ls, 0, -1), None);
        assert_eq!(point_offset(&ls, 2, 1), None);
        assert_eq!(point_offset(&ls, 1, 1), Some(c(2.0, 0.0)));
    }
}
