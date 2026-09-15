//! Step 1 of `Contours-to-Raster.md`: pulling Contours, Slope Lines, Jumps
//! and Heavy Objects out of a parsed map, building the Contour Raster, and
//! turning the supporting symbols into gravity evidence.

use geo::{Coord, LineString, Polygon};

use crate::contour_geometry::{self, coords_to_linestrings, nearest_index, resample_equal_chords};
use crate::contour_raster::{
    ContourRaster, CONTOUR_0_MATRIX_VALUE, HIGH_DENSITY, OUT_OF_BOUND, TEMPORARY_CONTOUR,
};
use crate::contour_symbols::{classify_symbol, jump_gravity_side, SymbolFamily};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{
    gravity_vector_for_side, Contour, LineGravityDefiners, LineWithGravity, PointGravityDefiners,
};
use crate::map::{Map, ObjectKind, Symbol};

/// `extract`'s own error: a plain message (no more Contour Raster crash to
/// diagnose -- a conflicting pixel is marked high density instead, see
/// `extract`'s raster-fill loop).
pub struct ExtractError(String);

impl ExtractError {
    /// The error's own human-readable message.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::fmt::Debug for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl From<String> for ExtractError {
    fn from(message: String) -> Self {
        ExtractError(message)
    }
}

/// Everything Step 1 produces: the contours (gravity still mostly
/// undefined), the Contour Raster they were written to, the gravity evidence
/// Step 2 and Step 3 consume, and any warnings along the way (a Slope Line
/// with no contour under it, a Jump with no derivable direction, a
/// degenerate circle fit).
pub struct Step1Result {
    /// One entry per contour subpath found on the map.
    pub contours: Vec<Contour>,
    /// Every contour's own raw, unprocessed node sequence (still curved,
    /// mm-on-paper converted straight to ground meters, no flattening or
    /// resampling) -- kept only for the `--create_svg` visualization's
    /// "before" picture. One entry per originally-digitized contour object,
    /// same as `contours` at the moment Step 1's raster fill finishes, but
    /// *not* index-parallel to it from then on: the Growing Process can
    /// merge two contours into one (shrinking `contours` by one), and does
    /// not also try to splice their two raw node sequences into a single,
    /// geometrically continuous one (there is no meaningful single curve
    /// through two originally-separate digitized objects) -- so after
    /// growing, this is simply every contour's own raw trace, drawn as its
    /// own separate SVG subpath, with no correspondence to `contours`'
    /// indices assumed or needed.
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
    /// Every Flying End's own position *before* the Growing Process ran --
    /// kept only for the `--create_svg` visualization's red rings (see
    /// `run_growing_process`).
    pub pre_growing_flying_ends: Vec<Coord<f64>>,
    /// Parallel to `contours`: whether that contour was touched by the
    /// Growing Process (grown, merged into, or both) -- kept only for
    /// `--create_svg`'s `01_..._step1_growing.svg`, which draws these in
    /// blue instead of green.
    pub grown_by_growing_process: Vec<bool>,
    /// One entry per case-(c) growing step actually taken (see
    /// `grow_one_step`), empty until `run_growing` runs -- kept only for
    /// `--create_svg`'s `01_..._step1_growing.svg`, which draws each step's
    /// own four push/pull contributions as separate colored vectors
    /// (`growing_visualization_push_pull_vectors_scale`-scaled).
    pub growing_push_pull_vectors: Vec<GrowingStepForces>,
    /// Recoverable problems found along the way.
    pub warnings: Vec<String>,
}

/// One case-(c) growing step's own flying-end position (the vectors' shared
/// tail, *before* the step) and the four separate push/pull contributions
/// [`growing_forces`] computes there ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)),
/// before they are summed into the step's actual direction ([`GrowingStepForces::total`]) --
/// kept only for `--create_svg`'s `01_..._step1_growing.svg` visualization.
#[derive(Clone, Copy, Debug)]
pub struct GrowingStepForces {
    /// The Flying End's own position before this step.
    pub flying_end: Coord<f64>,
    /// `growing_previous_distance_direction_weight`'s own contribution:
    /// `previous_direction`, scaled by that weight.
    pub previous_direction: (f64, f64),
    /// `growing_out_of_bound_direction_weight`'s own contribution -- always
    /// `(0.0, 0.0)` while `phase` is `MatchingEnds`, since the term is
    /// dropped entirely then (see `growing_forces`).
    pub out_of_bound: (f64, f64),
    /// `growing_density_direction_weight`'s own contribution.
    pub density: (f64, f64),
    /// `growing_other_contours_direction_weight`'s own contribution
    /// (negative weight, so this typically points away from nearby hits).
    pub other_contours: (f64, f64),
}

impl GrowingStepForces {
    /// The single direction [`next_grown_node`] actually steps along: the
    /// four contributions summed, exactly as `growing_direction` used to
    /// compute it directly.
    fn total(&self) -> (f64, f64) {
        (
            self.previous_direction.0
                + self.out_of_bound.0
                + self.density.0
                + self.other_contours.0,
            self.previous_direction.1
                + self.out_of_bound.1
                + self.density.1
                + self.other_contours.1,
        )
    }
}

/// One Slope Line found on the map: its own position and rotation, kept
/// (regardless of whether it resolved) for `--create_svg`'s benefit -- see
/// `Step1Result::slope_lines`.
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
    /// `Step1Result::point_definers` -- kept so `--create_svg` can tell the
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
/// Contour object's first. Left unmerged, Step 1 would treat the two pieces
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

/// Groups every contour pixel `poly` covers by contour index, returning one
/// `(contour_idx, centroid)` pair per contour touched -- not one per pixel,
/// since a buffered polygon commonly covers many pixels of the very same
/// nearby contour (its own width). A `BTreeMap`, not a `HashMap`, so the
/// order these come back in stays the same across runs. Used both for a
/// Heavy Object's own circle-fit readings and, since it must be called
/// *before* a Jump's polygon is stamped high density (which would otherwise
/// erase the very evidence of which contours were under it), for
/// `LineGravityDefiners::touched_contours`.
fn contour_centroids_in_polygon(
    raster: &ContourRaster,
    poly: &Polygon<f64>,
) -> Vec<(u64, Coord<f64>)> {
    let mut hits: std::collections::BTreeMap<u64, (Coord<f64>, usize)> =
        std::collections::BTreeMap::new();
    for (px, py) in raster.pixels_in_polygon(poly) {
        let val = raster.get(px, py);
        if val < CONTOUR_0_MATRIX_VALUE {
            continue;
        }
        let contour_idx = (val - CONTOUR_0_MATRIX_VALUE) as u64;
        let center = raster.pixel_center(px, py);
        let entry = hits
            .entry(contour_idx)
            .or_insert((Coord { x: 0.0, y: 0.0 }, 0));
        entry.0.x += center.x;
        entry.0.y += center.y;
        entry.1 += 1;
    }
    hits.into_iter()
        .map(|(idx, (sum, count))| {
            (
                idx,
                Coord {
                    x: sum.x / count as f64,
                    y: sum.y / count as f64,
                },
            )
        })
        .collect()
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

/// Runs Step 1: extracts Contours, Slope Lines, Jumps and Heavy Objects from
/// `map`, builds the Contour Raster, and turns the supporting symbols into
/// gravity evidence for Step 2 and Step 3.
pub fn extract(map: &Map, config: &Config) -> Result<Step1Result, ExtractError> {
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
        return Err(ExtractError(
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
                let idx = contours.len() as u64;
                raster.write_contour(idx, ls);
                contours.push(Contour {
                    lwg: LineWithGravity::new(ls.clone()),
                    elevation_height: None,
                });
                raw_polylines.push(raw_poly.clone());
            }
        }
    }
    if contours.is_empty() {
        return Err(ExtractError(
            "no Contour symbols (codes 101/102) were found on the map".to_string(),
        ));
    }

    // Step 1's flood-fill sub-step: a Hot Rain Drop Production and a Hot
    // Anti Rain Drop Production from every contour, marking every reachable
    // `UNDEFINED` pixel `NO_CONTOUR_IN_BOUND`. No contour has a real gravity
    // direction yet, so an arbitrary fixed placeholder side is used -- Rain
    // and Anti Rain together cover both perpendicular sides regardless of
    // which one is picked.
    for (idx, contour) in contours.iter().enumerate() {
        crate::step3_rain_drop::flood_fill_from_contour(
            &mut raster,
            &contour.lwg.ls,
            idx as u64,
            1.0,
            config,
        );
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
                    // Captured before the polygon's own area is stamped high
                    // density below, which would otherwise erase the very
                    // evidence (a contour's own raster value) of which
                    // contours were under it -- Step 2 uses this list
                    // instead of re-scanning the (by then high-density)
                    // raster itself.
                    let touched_contours = contour_centroids_in_polygon(&raster, &poly);
                    // A Jump is real terrain: the ground under and around it
                    // is marked high density, independently of whatever
                    // value it held before.
                    raster.mark_high_density_polygon(&poly);
                    line_definers.push(LineGravityDefiners {
                        lwg,
                        poly,
                        touched_contours,
                    });
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
                    for (contour_idx, at) in contour_centroids_in_polygon(&raster, &poly) {
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

    // Computed only now, after every Jump's own area has already been
    // stamped `HIGH_DENSITY` above: a Jump close to the map border must
    // already act as a firebreak here, or the border flood below would
    // leak straight through its (still-undefined) pixels into the map's
    // interior before the Jump ever got a chance to block it.
    raster.compute_out_of_bound();

    // Captured now, before the Growing Process runs (see `run_growing`,
    // called separately so `--create_svg` can write a "before" picture in
    // between): every open contour's own end not yet resolved to an
    // out-of-bound or high-density pixel.
    let pending = collect_flying_ends(&contours, &raster);
    let pre_growing_flying_ends: Vec<Coord<f64>> = pending
        .iter()
        .map(|e| flying_end_position(&contours[e.contour_idx].lwg.ls, e.is_start))
        .collect();
    let grown_by_growing_process = vec![false; contours.len()];

    Ok(Step1Result {
        contours,
        raw_polylines,
        raster,
        point_definers,
        line_definers,
        slope_lines,
        slope_lines_contours_search_radius: config.slope_lines_contours_search_radius,
        heavy_object_polygons,
        pre_growing_flying_ends,
        grown_by_growing_process,
        growing_push_pull_vectors: Vec::new(),
        warnings,
    })
}

/// One open contour's start (`is_start`) or end node not yet resolved to an
/// out-of-bound or high-density pixel (Step 1's Growing Process).
#[derive(Clone, Copy)]
struct FlyingEnd {
    contour_idx: usize,
    is_start: bool,
}

/// Which of the Growing Process's two passes [`grow_one_step`] is advancing
/// a Flying End through (see the doc). Two contours that run close and
/// parallel near the border can land in each other's window purely because
/// they're both, independently, trying to reach that same border -- not
/// because they're actually the same physical line split in two -- so every
/// Flying End first spends up to `growing_oob_seeking_max_steps` steps
/// (`SeekingOutOfBound`) reacting only to the raster itself, matching
/// against another Flying End disabled outright; only a Flying End that
/// hasn't reached the border within that budget falls through to
/// `MatchingEnds`, the full process, with matching restored and the
/// out-of-bound attraction term dropped (having failed to find a border on
/// its own, it's assumed to actually belong with a nearby Flying End
/// instead of still chasing one).
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrowingPhase {
    SeekingOutOfBound,
    MatchingEnds,
}

fn flying_end_position(ls: &LineString<f64>, is_start: bool) -> Coord<f64> {
    if is_start {
        ls.0[0]
    } else {
        *ls.0.last().unwrap()
    }
}

fn is_flying(raster: &ContourRaster, pos: Coord<f64>) -> bool {
    let (px, py) = raster.to_px(pos);
    let val = raster.get(px, py);
    val != OUT_OF_BOUND && val != HIGH_DENSITY
}

fn collect_flying_ends(contours: &[Contour], raster: &ContourRaster) -> Vec<FlyingEnd> {
    let mut ends = Vec::new();
    for (i, c) in contours.iter().enumerate() {
        let ls = &c.lwg.ls;
        if ls.is_closed() || ls.0.len() < 2 {
            continue;
        }
        for &is_start in &[true, false] {
            if is_flying(raster, flying_end_position(ls, is_start)) {
                ends.push(FlyingEnd {
                    contour_idx: i,
                    is_start,
                });
            }
        }
    }
    ends
}

/// The unit vector of `ls`'s own last segment, in the direction the Growing
/// Process should continue past `is_start`'s own end (away from the
/// contour's body).
fn previous_direction(ls: &LineString<f64>, is_start: bool) -> (f64, f64) {
    let n = ls.0.len();
    let (from, to) = if is_start {
        (ls.0[1], ls.0[0])
    } else {
        (ls.0[n - 2], ls.0[n - 1])
    };
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    if len < 1e-12 {
        (1.0, 0.0)
    } else {
        (dx / len, dy / len)
    }
}

enum WindowPixelKind {
    OutOfBound,
    HighDensity,
    Contour,
}

/// Every out-of-bound, high-density, or contour pixel's world-space center
/// found around `center_px` (Appendix 5), each kind checked against its own
/// square window: a `Contour` pixel (a real one, or a `TEMPORARY_CONTOUR`
/// tail -- some Flying End's own not-yet-final tail, see
/// [`ContourRaster::mark_temporary_step`], counts as one here too, so two
/// Flying Ends growing at the same time repel each other's tails instead of
/// only reacting to already-finalized contours) only counts between 2 and
/// `2*half_contours+1` pixels of `center_px` -- `center_px` itself and its 8
/// immediate neighbors are always excluded from repulsion, since the Flying
/// End always sits right on top of its own just-written body there, and
/// that self-proximity would otherwise swamp the term with a huge,
/// meaningless push instead of reflecting genuinely nearby contour pixels.
/// An `OutOfBound`/`HighDensity` pixel only counts within
/// `2*half_attractions+1` pixels, no such exclusion. Both windows are
/// scanned in one pass, over their shared (larger) bounding box, each pixel
/// then kept or dropped by its own kind's own radius (Chebyshev distance,
/// i.e. `max(|dx|, |dy|)`, matching each window's own square shape).
fn growing_window_hits(
    raster: &ContourRaster,
    center_px: (i64, i64),
    half_contours: i64,
    half_attractions: i64,
) -> Vec<(WindowPixelKind, Coord<f64>)> {
    let half = half_contours.max(half_attractions);
    let mut hits = Vec::new();
    for y in (center_px.1 - half)..=(center_px.1 + half) {
        for x in (center_px.0 - half)..=(center_px.0 + half) {
            let cheby = (x - center_px.0).abs().max((y - center_px.1).abs());
            let val = raster.get(x, y);
            let kind = if val == OUT_OF_BOUND {
                if cheby > half_attractions {
                    continue;
                }
                WindowPixelKind::OutOfBound
            } else if val == HIGH_DENSITY {
                if cheby > half_attractions {
                    continue;
                }
                WindowPixelKind::HighDensity
            } else if val == TEMPORARY_CONTOUR || val >= CONTOUR_0_MATRIX_VALUE {
                if cheby <= 1 || cheby > half_contours {
                    continue;
                }
                WindowPixelKind::Contour
            } else {
                continue;
            };
            hits.push((kind, raster.pixel_center(x, y)));
        }
    }
    hits
}

/// Appendix 5's weighted attraction/repulsion, broken down by term rather
/// than pre-summed: `1`/`3` pixels attract, any contour pixel repels,
/// blended with the contour's own previous heading. Once a Flying End has
/// moved on to `MatchingEnds` (see [`GrowingPhase`]), the out-of-bound term
/// is dropped entirely -- it's already had its dedicated, matching-free
/// budget to reach the border on its own and didn't, so no longer being
/// pulled toward one lets it settle into matching another nearby Flying End
/// instead. The breakdown itself (rather than just [`GrowingStepForces::total`])
/// is kept only so `grow_one_step` can record it into
/// `Step1Result::growing_push_pull_vectors` for `--create_svg`'s benefit --
/// the Growing Process itself only ever needs the summed direction.
fn growing_forces(
    flying_end: Coord<f64>,
    previous_direction: (f64, f64),
    hits: &[(WindowPixelKind, Coord<f64>)],
    config: &Config,
    phase: GrowingPhase,
) -> GrowingStepForces {
    let mut forces = GrowingStepForces {
        flying_end,
        previous_direction: (
            config.growing_previous_distance_direction_weight * previous_direction.0,
            config.growing_previous_distance_direction_weight * previous_direction.1,
        ),
        out_of_bound: (0.0, 0.0),
        density: (0.0, 0.0),
        other_contours: (0.0, 0.0),
    };
    for (kind, center) in hits {
        if phase == GrowingPhase::MatchingEnds && matches!(kind, WindowPixelKind::OutOfBound) {
            continue;
        }
        let (bx, by) = (center.x - flying_end.x, center.y - flying_end.y);
        let d = bx.hypot(by);
        if d < 1e-12 {
            continue; // the pixel sits exactly on the Flying End
        }
        let w = match kind {
            WindowPixelKind::OutOfBound => config.growing_out_of_bound_direction_weight,
            WindowPixelKind::HighDensity => config.growing_density_direction_weight,
            WindowPixelKind::Contour => config.growing_other_contours_direction_weight,
        };
        let term = match kind {
            WindowPixelKind::OutOfBound => &mut forces.out_of_bound,
            WindowPixelKind::HighDensity => &mut forces.density,
            WindowPixelKind::Contour => &mut forces.other_contours,
        };
        term.0 += w * bx / (d * d);
        term.1 += w * by / (d * d);
    }
    forces
}

/// The next grown node, `step` away from `flying_end` along `direction`, or
/// straight ahead along `previous_direction` if `direction` came out zero
/// (the window held nothing to react to).
fn next_grown_node(
    flying_end: Coord<f64>,
    previous_direction: (f64, f64),
    direction: (f64, f64),
    step: f64,
) -> Coord<f64> {
    let len = direction.0.hypot(direction.1);
    let (ux, uy) = if len < 1e-12 {
        previous_direction
    } else {
        (direction.0 / len, direction.1 / len)
    };
    Coord {
        x: flying_end.x + ux * step,
        y: flying_end.y + uy * step,
    }
}

fn append_node(contours: &mut [Contour], contour_idx: usize, is_start: bool, node: Coord<f64>) {
    let ls = &mut contours[contour_idx].lwg.ls;
    if is_start {
        ls.0.insert(0, node);
    } else {
        ls.0.push(node);
    }
}

/// Closes `contour_idx`'s own `ls` into a ring (Step 1's Growing Process,
/// case (a), when the "other Flying End" the window found turns out to be
/// this same contour's own other end): appends a copy of that other end's
/// own current position -- exactly what "set the new node on top of it"
/// means here, since there is only one contour's worth of nodes to place it
/// on top of -- so the first and last `Coord` end up identical, then
/// re-samples the whole thing as the closed contour it now is (Appendix 1
/// shrinks the step to evenly divide the perimeter instead of leaving a
/// short closing segment).
///
/// That perimeter-wide re-derivation can shift *every* node, not just the
/// newly closed one -- including ones from the contour's own original body,
/// already drawn into the Contour Raster long before growing ever started --
/// so its whole previous footprint is cleared first (see
/// [`ContourRaster::clear_contour`]) rather than drawing the new one
/// additively on top of the old.
fn close_contour(
    contour_idx: usize,
    is_start: bool,
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    config: &Config,
    grown: &mut [bool],
) {
    let other_end_pos = flying_end_position(&contours[contour_idx].lwg.ls, !is_start);
    append_node(contours, contour_idx, is_start, other_end_pos);
    let closed = resample_equal_chords(&contours[contour_idx].lwg.ls, config.contours_step);
    contours[contour_idx].lwg.ls = closed.clone();
    raster.clear_contour(contour_idx as u64);
    raster.write_contour(contour_idx as u64, &closed);
    grown[contour_idx] = true;
}

/// Merges the contour at `a_idx` (its Flying End at `a_is_start`) with the
/// one at `b_idx` (`b_is_start`), which the Growing Process found close
/// enough to snap onto (Step 1's Growing Process, case (a)). Keeps the
/// smaller of the two indices (an arbitrary but deterministic pick -- either
/// choice satisfies the doc's own "choose randomly one of the two"), and
/// re-numbers every reference to the discarded index -- the Contours vector,
/// every already-written Contour Raster pixel, every pending Flying End, and
/// every already-collected `PointGravityDefiners.reference_contour`.
#[allow(clippy::too_many_arguments)]
fn merge_contours(
    a_idx: usize,
    a_is_start: bool,
    b_idx: usize,
    b_is_start: bool,
    contours: &mut Vec<Contour>,
    point_definers: &mut [PointGravityDefiners],
    pending: &mut std::collections::VecDeque<FlyingEnd>,
    grown: &mut Vec<bool>,
    raster: &mut ContourRaster,
    config: &Config,
) {
    let (keep_idx, keep_is_start, remove_idx) = if a_idx < b_idx {
        (a_idx, a_is_start, b_idx)
    } else {
        (b_idx, b_is_start, a_idx)
    };
    let remove_is_start = if a_idx < b_idx {
        b_is_start
    } else {
        a_is_start
    };

    let mut merged_pts = contours[keep_idx].lwg.ls.0.clone();
    if keep_is_start {
        merged_pts.reverse();
    }
    let mut tail_pts = contours[remove_idx].lwg.ls.0.clone();
    if !remove_is_start {
        tail_pts.reverse();
    }
    // The Growing Process placed both Flying Ends at (effectively) the same
    // position -- drop the duplicate rather than keep a zero-length segment.
    if !tail_pts.is_empty() {
        tail_pts.remove(0);
    }
    merged_pts.extend(tail_pts);
    let merged_ls = resample_equal_chords(&LineString::new(merged_pts), config.contours_step);

    contours[keep_idx].lwg.ls = merged_ls.clone();
    contours.remove(remove_idx);

    // The new joint segment's own (arbitrary) length can shift every node
    // downstream of it out of phase with whichever pixels its own original,
    // already-drawn position claimed -- not just the two contours' own
    // Flying Ends -- so both contours' whole previous footprints are
    // cleared before the merged, resampled `ls` is drawn fresh, the same
    // reasoning as `close_contour`'s own (see its doc comment).
    raster.clear_contour(keep_idx as u64);
    raster.clear_contour(remove_idx as u64);
    raster.merge_contour_indices(keep_idx as u64, remove_idx as u64);
    raster.write_contour(keep_idx as u64, &merged_ls);

    for pd in point_definers.iter_mut() {
        if pd.reference_contour == remove_idx as u64 {
            pd.reference_contour = keep_idx as u64;
        } else if pd.reference_contour > remove_idx as u64 {
            pd.reference_contour -= 1;
        }
    }
    // `merged_pts` is always built as [keep's surviving end ... join ...
    // remove's surviving end] -- whichever of keep's two ends didn't just
    // merge always ends up at the front (index 0), and whichever of remove's
    // two ends didn't just merge always ends up at the back, regardless of
    // `keep_is_start`/`remove_is_start` (those only control which raw point
    // list gets reversed to make that true). So any *other* Flying End still
    // waiting in `pending` on one of these same two contours -- not the ones
    // just consumed by this merge, already removed from `pending` by the
    // caller -- needs remapping onto that fixed shape, not just an index
    // shift: keep's own other end always becomes the new start, and remove's
    // own other end always becomes the new end of the merged contour (at
    // `keep_idx`, since `remove_idx` no longer exists).
    for p in pending.iter_mut() {
        if p.contour_idx == keep_idx {
            p.is_start = true;
        } else if p.contour_idx == remove_idx {
            p.contour_idx = keep_idx;
            p.is_start = false;
        } else if p.contour_idx > remove_idx {
            p.contour_idx -= 1;
        }
    }
    grown[keep_idx] = true;
    grown.remove(remove_idx);
}

/// A generous cap on the *total* number of growth steps taken across every
/// Flying End combined, scaled by how many there were to start with --
/// purely a safety valve against an unbounded loop (e.g. an oscillating
/// limit cycle between two density clusters), not part of the doc's own
/// algorithm, which assumes every path eventually reaches the out-of-bound
/// ring.
const MAX_GROWING_STEPS_PER_END: u64 = 100_000;

/// What one call to [`grow_one_step`] did to the Flying End it was given.
enum GrowStepOutcome {
    /// Resolved (landed on an out-of-bound/high-density pixel, or merged
    /// with another Flying End) -- nothing left to grow.
    Resolved,
    /// Still flying after this one step; here's its new position.
    StillFlying(FlyingEnd),
}

/// Advances one Flying End by exactly one step of the Growing Process (case
/// (a), (b), or (c) -- see the doc). `pending` is every *other* Flying End
/// still waiting on its own next step (`end` itself is not in it -- the
/// caller already popped it off before calling this); ignored entirely
/// while `phase` is `SeekingOutOfBound`, since case (a) is skipped then.
#[allow(clippy::too_many_arguments)]
fn grow_one_step(
    end: FlyingEnd,
    pending: &mut std::collections::VecDeque<FlyingEnd>,
    contours: &mut Vec<Contour>,
    point_definers: &mut [PointGravityDefiners],
    grown: &mut Vec<bool>,
    raster: &mut ContourRaster,
    config: &Config,
    phase: GrowingPhase,
    push_pull_vectors: &mut Vec<GrowingStepForces>,
) -> GrowStepOutcome {
    let (contour_idx, is_start) = (end.contour_idx, end.is_start);
    let half_contours = ((config.growing_window_size_px_contours / 2) as i64).max(1);
    let half_attractions = ((config.growing_window_size_px_attractions / 2) as i64).max(1);

    let pos = flying_end_position(&contours[contour_idx].lwg.ls, is_start);
    if !is_flying(raster, pos) {
        return GrowStepOutcome::Resolved;
    }
    let (px, py) = raster.to_px(pos);

    // (a) another pending Flying End inside the window? Skipped outright
    // while still seeking the out-of-bound area on its own (see
    // `GrowingPhase`) -- two contours running close and parallel near the
    // border must each reach it independently, not snap onto each other
    // just because they happen to sit in each other's window. Uses the
    // attraction window (`half_attractions`), grouped with the
    // out-of-bound/high-density pull terms as another thing the Growing
    // Process can move toward, rather than the contour-repulsion window.
    if phase == GrowingPhase::MatchingEnds {
        let x_range = (px - half_attractions)..=(px + half_attractions);
        let y_range = (py - half_attractions)..=(py + half_attractions);
        let other = pending.iter().position(|o| {
            let other_pos = flying_end_position(&contours[o.contour_idx].lwg.ls, o.is_start);
            let (ox, oy) = raster.to_px(other_pos);
            x_range.contains(&ox) && y_range.contains(&oy)
        });
        if let Some(i) = other {
            let other_end = pending.remove(i).expect("index just found by position()");
            if other_end.contour_idx == contour_idx {
                // The other end found is this same contour's own other end
                // (its only other possible flying end, so no self-match
                // ambiguity beyond this): close it into a ring instead of
                // merging it with a second contour, then equal-chord
                // resample it the way any closed contour is (Appendix 1
                // shrinks the step to evenly divide the perimeter, rather
                // than leaving a short closing segment).
                close_contour(contour_idx, is_start, contours, raster, config, grown);
            } else {
                merge_contours(
                    contour_idx,
                    is_start,
                    other_end.contour_idx,
                    other_end.is_start,
                    contours,
                    point_definers,
                    pending,
                    grown,
                    raster,
                    config,
                );
            }
            return GrowStepOutcome::Resolved;
        }
    }

    let hits = growing_window_hits(raster, (px, py), half_contours, half_attractions);

    // (b) nearest out-of-bound/high-density pixel closer than this step's
    // own length (contours_step * growing_step_length) -- the same
    // distance case (c) would otherwise move by, so a pixel case (c) would
    // already land on or past is settled on directly here instead.
    let step_length = config.contours_step * config.growing_step_length;
    let mut nearest: Option<(f64, Coord<f64>)> = None;
    for (kind, center) in &hits {
        if matches!(
            kind,
            WindowPixelKind::OutOfBound | WindowPixelKind::HighDensity
        ) {
            let d = (center.x - pos.x).hypot(center.y - pos.y);
            if d < step_length && nearest.as_ref().is_none_or(|&(bd, _)| d < bd) {
                nearest = Some((d, *center));
            }
        }
    }
    if let Some((_, target)) = nearest {
        append_node(contours, contour_idx, is_start, target);
        let resampled = resample_equal_chords(&contours[contour_idx].lwg.ls, config.contours_step);
        contours[contour_idx].lwg.ls = resampled.clone();
        // As in `close_contour`: resampling can in principle shift nodes
        // other than the newly snapped one, so the contour's previous
        // footprint is cleared before redrawing it fresh, rather than
        // drawn additively on top of whatever was there before.
        raster.clear_contour(contour_idx as u64);
        raster.write_contour(contour_idx as u64, &resampled);
        grown[contour_idx] = true;
        return GrowStepOutcome::Resolved;
    }

    // (c) attraction/repulsion direction. Not written under this contour's
    // own real index yet while still flying (see `close_contour`'s own doc
    // comment for why: only once this contour's `ls` reaches its actual
    // final shape -- here, or in case (a)/(b) above -- is it drawn under
    // that index, in one shot, so the raster never ends up holding pixels
    // from an intermediate, not-yet-final position under a real contour's
    // value). The step just taken is, however, marked `TEMPORARY_CONTOUR`
    // right away, so a different Flying End growing in parallel repels off
    // of it instead of being blind to it -- otherwise two contours each
    // independently seeking the border, close and parallel, can cross one
    // another unnoticed (neither has written anything real yet for the
    // other to react to).
    let prev_dir = previous_direction(&contours[contour_idx].lwg.ls, is_start);
    let forces = growing_forces(pos, prev_dir, &hits, config, phase);
    let dir = forces.total();
    push_pull_vectors.push(forces);
    let next = next_grown_node(pos, prev_dir, dir, step_length);
    raster.mark_temporary_step(pos, next);
    append_node(contours, contour_idx, is_start, next);
    grown[contour_idx] = true;
    if is_flying(raster, next) {
        GrowStepOutcome::StillFlying(FlyingEnd {
            contour_idx,
            is_start,
        })
    } else {
        // Landed directly on an out-of-bound/high-density pixel by chance,
        // rather than being snapped there by case (b): this `ls` is now
        // final too, so it gets its one, whole-`ls` write here.
        raster.write_contour(contour_idx as u64, &contours[contour_idx].lwg.ls);
        GrowStepOutcome::Resolved
    }
}

/// Step 1's final sub-step (see the doc): extends every open contour's
/// Flying End until it resolves to an out-of-bound or high-density pixel,
/// in two passes -- see [`GrowingPhase`]. Within each pass, Flying Ends are
/// never advanced in parallel, but round-robin rather than one at a time to
/// completion: every still-pending Flying End gets exactly one growth step,
/// then the whole list is cycled through again, and so on until none are
/// left (resolving one, by merging two contours together, can also resolve
/// another already in the list -- `MatchingEnds` only).
///
/// Deliberately not run as part of `extract` itself: `--create_svg` needs to
/// write `00_..._step1.svg` (the pre-growing state, with `result`'s own
/// `pre_growing_flying_ends` as red rings) before this runs, then
/// `01_..._step1_growing.svg` (this function's own result) after -- see the
/// doc's Visualization section. Sets `result.grown_by_growing_process` (used
/// by `01_..._step1_growing.svg` to draw a touched contour in blue instead
/// of green) and returns any new warnings raised along the way (`MatchingEnds`'
/// own step budget, `MAX_GROWING_STEPS_PER_END` times however many Flying
/// Ends entered that pass, running out before every one resolved).
pub fn run_growing(result: &mut Step1Result, config: &Config) -> Vec<String> {
    let mut grown = vec![false; result.contours.len()];
    let mut warnings = Vec::new();
    let mut push_pull_vectors = Vec::new();

    // Phase 1 (`SeekingOutOfBound`): every Flying End gets up to
    // `growing_oob_seeking_max_steps` steps entirely on its own -- no
    // merging, no closing, `contours`/`grown` never change length here, so
    // tracking each end's own step count by its (stable) `(contour_idx,
    // is_start)` is safe for the whole pass. A budget of `0` turns this
    // phase off outright: every Flying End starts straight in `MatchingEnds`.
    let initial_ends: std::collections::VecDeque<FlyingEnd> =
        collect_flying_ends(&result.contours, &result.raster).into();
    let seek_phase_enabled = config.growing_oob_seeking_max_steps > 0;
    let mut seeking = if seek_phase_enabled {
        initial_ends.clone()
    } else {
        std::collections::VecDeque::new()
    };
    let mut steps_taken: std::collections::HashMap<(usize, bool), u64> =
        std::collections::HashMap::new();
    let mut unused_pending: std::collections::VecDeque<FlyingEnd> =
        std::collections::VecDeque::new();
    let mut matching = if seek_phase_enabled {
        std::collections::VecDeque::new()
    } else {
        initial_ends
    };
    while let Some(end) = seeking.pop_front() {
        match grow_one_step(
            end,
            &mut unused_pending,
            &mut result.contours,
            &mut result.point_definers,
            &mut grown,
            &mut result.raster,
            config,
            GrowingPhase::SeekingOutOfBound,
            &mut push_pull_vectors,
        ) {
            GrowStepOutcome::Resolved => {}
            GrowStepOutcome::StillFlying(new_end) => {
                let count = steps_taken
                    .entry((new_end.contour_idx, new_end.is_start))
                    .or_insert(0);
                *count += 1;
                if *count >= config.growing_oob_seeking_max_steps {
                    matching.push_back(new_end);
                } else {
                    seeking.push_back(new_end);
                }
            }
        }
    }

    // Phase 2 (`MatchingEnds`): the full process for whatever didn't reach
    // the border on its own within phase 1's budget.
    let mut pending = matching;
    let max_steps = MAX_GROWING_STEPS_PER_END * pending.len().max(1) as u64;
    let mut steps_taken = 0u64;
    while let Some(end) = pending.pop_front() {
        match grow_one_step(
            end,
            &mut pending,
            &mut result.contours,
            &mut result.point_definers,
            &mut grown,
            &mut result.raster,
            config,
            GrowingPhase::MatchingEnds,
            &mut push_pull_vectors,
        ) {
            GrowStepOutcome::Resolved => {}
            GrowStepOutcome::StillFlying(new_end) => pending.push_back(new_end),
        }
        steps_taken += 1;
        if steps_taken >= max_steps {
            warnings.push(format!(
                "the Growing Process did not resolve every Flying End within its step budget \
                 ({max_steps} steps total); {} left unresolved",
                pending.len()
            ));
            break;
        }
    }
    // Every remaining `TEMPORARY_CONTOUR` pixel is a stretch of tail some
    // Flying End tried and abandoned along the way (e.g. resampling after a
    // merge or a close moved its nodes elsewhere) -- swept back to
    // `NO_CONTOUR_IN_BOUND` only now that both passes are done and no
    // still-flying neighbor could still need it as a repeller.
    result.raster.clear_temporary_contours();
    result.grown_by_growing_process = grown;
    result.growing_push_pull_vectors = push_pull_vectors;
    warnings
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
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 0,
            growing_window_size_px_contours: 4,
            growing_window_size_px_attractions: 4,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
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
    fn contour_centroids_in_polygon_survives_a_later_high_density_stamp() {
        // Mirrors the order Jump handling uses: capture which contours a
        // polygon covers *before* marking that polygon's own area high
        // density, since that stamp would otherwise erase the very evidence
        // (a plain contour value in the raster) of which contour was there
        // -- exactly what used to make Step 2 find nothing under a Jump.
        let ls = LineString::new(vec![c(0.0, 5.0), c(20.0, 5.0)]);
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);

        let poly = Polygon::new(
            LineString::new(vec![
                c(8.0, 0.0),
                c(12.0, 0.0),
                c(12.0, 10.0),
                c(8.0, 10.0),
                c(8.0, 0.0),
            ]),
            vec![],
        );

        let touched = contour_centroids_in_polygon(&raster, &poly);
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].0, 0);

        raster.mark_high_density_polygon(&poly);
        // The polygon's own area is now high density, not contour 0 -- the
        // state that used to make a later, raster-based re-scan find
        // nothing.
        let (px, py) = raster.to_px(touched[0].1);
        assert_eq!(raster.get(px, py), crate::contour_raster::HIGH_DENSITY);
        // The already-captured evidence itself is unaffected, since it was
        // read before the stamp, not re-derived from the (by now
        // corrupted) raster.
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].0, 0);
    }

    #[test]
    fn extract_marks_a_raster_conflict_high_density_instead_of_crashing() {
        use crate::map::{Coord as MapCoord, LineSymbol, Object, PathObject};

        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 0,
            growing_window_size_px_contours: 4,
            growing_window_size_px_attractions: 4,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };

        let contour_symbol = Symbol::Line(LineSymbol {
            code: "101".to_string(),
            ..Default::default()
        });

        // Two straight, parallel contour pieces 0.05m apart -- close enough
        // that a 0.5m Contour Raster pixel written by both collides.
        let piece_a = Object {
            kind: ObjectKind::Path(PathObject::default()),
            symbol_id: 0,
            symbol_index: Some(0),
            coords: vec![MapCoord::new(0.0, 0.0, 0), MapCoord::new(10.0, 0.0, 0)],
            rotation: 0.0,
        };
        let piece_b = Object {
            kind: ObjectKind::Path(PathObject::default()),
            symbol_id: 0,
            symbol_index: Some(0),
            coords: vec![MapCoord::new(0.0, 0.05, 0), MapCoord::new(10.0, 0.05, 0)],
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

        // Must not crash: the doc's old crash-on-conflict path is gone.
        let result = extract(&map, &config).unwrap();
        assert_eq!(
            result.contours.len(),
            2,
            "both pieces stay separate contours"
        );

        // At least one pixel along the shared run must have been marked
        // high density rather than silently claimed by whichever contour
        // happened to write it second.
        let has_high_density = (0..result.raster.height as i64).any(|y| {
            (0..result.raster.width as i64)
                .any(|x| result.raster.get(x, y) == crate::contour_raster::HIGH_DENSITY)
        });
        assert!(has_high_density, "expected at least one high-density pixel");
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

    #[test]
    fn growing_closes_a_contour_whose_own_two_flying_ends_meet_each_other() {
        // A wide, shallow "V": both ends sit at y=10, only 4m apart --
        // comfortably inside each other's growing window -- and they belong
        // to the very same (single) contour. Case (a) should close it into
        // a ring (not corrupt it by feeding both into `merge_contours` as
        // if they were two different contours, and not silently ignore the
        // match either -- a contour's own other end is a valid, and good,
        // case-(a) partner).
        let ls = LineString::new(vec![c(15.0, 10.0), c(17.0, 15.0), c(19.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);
        raster.compute_out_of_bound();
        let contour = Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
        };

        let mut result = Step1Result {
            contours: vec![contour],
            raw_polylines: vec![Vec::new()],
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            grown_by_growing_process: vec![false],
            growing_push_pull_vectors: Vec::new(),
            warnings: Vec::new(),
        };
        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 5.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 0,
            growing_window_size_px_contours: 10,
            growing_window_size_px_attractions: 10,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };

        let warnings = run_growing(&mut result, &config);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            result.contours.len(),
            1,
            "still one contour -- closed, not merged away or duplicated"
        );
        assert!(
            result.contours[0].lwg.ls.is_closed(),
            "expected the contour to have been closed into a ring, got {:?}",
            result.contours[0].lwg.ls
        );
        assert_eq!(result.grown_by_growing_process, vec![true]);
    }

    #[test]
    fn growing_merge_keeps_the_two_contours_raw_polylines_separate() {
        // Two short, separate open contours whose near ends (A's end, B's
        // start) are only 2m apart -- well inside each other's growing
        // window -- while each contour's own two ends stay 10m apart, safely
        // outside it, so this merges A with B rather than either closing on
        // itself. A's own far end and B's own far end each just grow
        // straight toward the raster's border and resolve there.
        let ls_a = LineString::new(vec![c(0.0, 10.0), c(10.0, 10.0)]);
        let ls_b = LineString::new(vec![c(12.0, 10.0), c(22.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls_a);
        raster.write_contour(1, &ls_b);
        raster.compute_out_of_bound();

        let raw_a = vec![
            contour_geometry::RawVertex {
                coord: c(0.0, 10.0),
                is_curve_start: false,
            },
            contour_geometry::RawVertex {
                coord: c(10.0, 10.0),
                is_curve_start: false,
            },
        ];
        let raw_b = vec![
            contour_geometry::RawVertex {
                coord: c(12.0, 10.0),
                is_curve_start: false,
            },
            contour_geometry::RawVertex {
                coord: c(22.0, 10.0),
                is_curve_start: false,
            },
        ];

        let mut result = Step1Result {
            contours: vec![
                Contour {
                    lwg: LineWithGravity::new(ls_a),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_b),
                    elevation_height: None,
                },
            ],
            raw_polylines: vec![raw_a.clone(), raw_b.clone()],
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            grown_by_growing_process: vec![false, false],
            growing_push_pull_vectors: Vec::new(),
            warnings: Vec::new(),
        };
        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 5.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 0,
            growing_window_size_px_contours: 10,
            growing_window_size_px_attractions: 10,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };

        let warnings = run_growing(&mut result, &config);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(result.contours.len(), 1, "A and B merged into one contour");
        // Both originally-separate raw traces must survive as their own,
        // separate SVG subpaths -- never spliced into one, which would draw
        // a false connecting segment straight across the gap between them
        // (there is no meaningful single curve through two originally
        // distinct digitized objects).
        assert_eq!(result.raw_polylines.len(), 2);
        assert!(result.raw_polylines.contains(&raw_a));
        assert!(result.raw_polylines.contains(&raw_b));
    }

    #[test]
    fn growing_seeking_phase_reaches_the_border_instead_of_matching_a_nearby_contour() {
        // Two separate contours, close and parallel (2m apart), with
        // nothing else drawn anywhere else on this 40x40 raster: once
        // `compute_out_of_bound` runs, virtually every pixel that isn't on
        // one of the two lines is out of bound, including plenty right
        // beside each contour's own end. Each end's *own* nearest
        // out-of-bound pixel (about 1m away, immediately off the line) is
        // much closer than the other contour's end (2m away) -- but the old,
        // single-phase code checked case (a) (matching against a nearby
        // Flying End) before ever looking for one, so it merged these two
        // anyway, despite each having a perfectly good border of its own
        // right there. With `growing_oob_seeking_max_steps` giving both ends
        // a matching-free first pass, each must instead resolve on its own.
        let ls_a = LineString::new(vec![c(5.0, 10.0), c(15.0, 10.0)]);
        let ls_b = LineString::new(vec![c(5.0, 12.0), c(15.0, 12.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls_a);
        raster.write_contour(1, &ls_b);
        raster.compute_out_of_bound();

        let mut result = Step1Result {
            contours: vec![
                Contour {
                    lwg: LineWithGravity::new(ls_a),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_b),
                    elevation_height: None,
                },
            ],
            raw_polylines: vec![Vec::new(), Vec::new()],
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            grown_by_growing_process: vec![false, false],
            growing_push_pull_vectors: Vec::new(),
            warnings: Vec::new(),
        };
        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 3.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 5,
            growing_window_size_px_contours: 6,
            growing_window_size_px_attractions: 6,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };

        let warnings = run_growing(&mut result, &config);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            result.contours.len(),
            2,
            "A and B each had their own out-of-bound pixel right there -- neither should have \
             matched the other instead"
        );
    }

    #[test]
    fn growing_step_is_deflected_by_another_flying_ends_temporary_tail() {
        // Case (c) during `SeekingOutOfBound`: a Flying End heading due
        // east, with no permanent contour or out-of-bound/high-density
        // pixel anywhere nearby -- so, absent any other hit, it just
        // continues dead straight (unaffected by `previous_direction`'s own
        // weight, since it's the only term). Two otherwise-identical runs,
        // the only difference being whether some *other* Flying End's own
        // not-yet-final tail happens to sit just ahead and to one side,
        // marked `TEMPORARY_CONTOUR` by `mark_temporary_step` exactly as
        // Step 1's Growing Process itself does for every step it takes
        // while still flying (see `grow_one_step`, case (c)). Before this
        // was wired up, a Flying End's own tail was invisible to any other
        // Flying End growing alongside it until it finally resolved -- two
        // contours seeking the border independently, close and parallel,
        // could fly right through each other. With it, the second run's new
        // node must swing measurably away from that pixel instead of
        // continuing on the same straight line as the first.
        fn grow_east_once(mark_temporary_pixel: bool) -> Coord<f64> {
            // Pixel-center coordinates throughout (as the rest of the
            // codebase's own tests do, e.g. `contour_raster.rs`'s): a Flying
            // End's own position is always its `ls`'s last point, which is
            // therefore always among the pixels its own body just wrote --
            // landing it exactly on that pixel's center (rather than an
            // arbitrary offset toward one corner) makes that self-distance
            // exactly `0`, which `growing_direction` already skips, instead
            // of contributing an incidental, corner-biased nudge that has
            // nothing to do with what this test is actually checking.
            let ls = LineString::new(vec![c(10.5, 50.5), c(16.5, 50.5)]);
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
            raster.write_contour(0, &ls);
            let mut interior = Vec::new();
            for y in 1..(raster.height as i64 - 1) {
                for x in 1..(raster.width as i64 - 1) {
                    interior.push((x, y));
                }
            }
            raster.commit_flood_pixels(&interior);
            raster.compute_out_of_bound();
            if mark_temporary_pixel {
                // Simulates some other Flying End's own last grow step,
                // landing just ahead of this one and one pixel above its
                // straight-line path.
                raster.mark_temporary_step(c(19.5, 51.5), c(20.0, 51.9));
            }

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let config = Config {
                bezier_linearization_step: 0.1,
                contours_step: 3.0,
                rasterization_px_size: 1.0,
                heavy_object_width: 1.0,
                heavy_object_growing: 0.2,
                circumference_fitting_points_number: 4,
                slope_lines_contours_search_radius: 3.0,
                rain_drop_step: 0.25,
                sources_per_contour_segment: 3,
                rain_drop_starting_voting_hysteresis: 3,
                undefined_gravity_vote_threshold: 0.8,
                growing_oob_seeking_max_steps: 10,
                growing_window_size_px_contours: 6,
                growing_window_size_px_attractions: 6,
                growing_step_length: 1.0,
                growing_previous_distance_direction_weight: 1.0,
                growing_out_of_bound_direction_weight: 1.0,
                growing_density_direction_weight: 1.0,
                growing_other_contours_direction_weight: -1.0,
                growing_visualization_push_pull_vectors_scale: 1.0,
            };
            let outcome = grow_one_step(
                FlyingEnd {
                    contour_idx: 0,
                    is_start: false,
                },
                &mut pending,
                &mut contours,
                &mut point_definers,
                &mut grown,
                &mut raster,
                &config,
                GrowingPhase::SeekingOutOfBound,
                &mut Vec::new(),
            );
            match outcome {
                GrowStepOutcome::StillFlying(_) => *contours[0].lwg.ls.0.last().unwrap(),
                GrowStepOutcome::Resolved => panic!("expected it to still be flying"),
            }
        }

        let straight = grow_east_once(false);
        assert!(
            (straight.y - 50.5).abs() < 1e-6,
            "with nothing nearby, the step should continue dead straight: {straight:?}"
        );
        let deflected = grow_east_once(true);
        assert!(
            deflected.y < 50.5 - 0.05,
            "a repelling TEMPORARY_CONTOUR pixel just above the straight path should have \
             pushed the next node measurably below it, got {deflected:?}"
        );
    }

    #[test]
    fn growing_step_marks_its_own_new_segment_temporary_while_still_flying() {
        // Case (c) itself, on the raster it actually runs against (not the
        // hand-simulated stand-in the previous test uses): once a step
        // leaves a Flying End still flying, the segment it just grew must
        // already read back as `TEMPORARY_CONTOUR`, or a second Flying End
        // scanning its own window a moment later would find nothing there
        // to repel from at all.
        let ls = LineString::new(vec![c(10.5, 50.5), c(16.5, 50.5)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
        raster.write_contour(0, &ls);
        let mut interior = Vec::new();
        for y in 1..(raster.height as i64 - 1) {
            for x in 1..(raster.width as i64 - 1) {
                interior.push((x, y));
            }
        }
        raster.commit_flood_pixels(&interior);
        raster.compute_out_of_bound();

        let mut contours = vec![Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
        }];
        let mut point_definers = Vec::new();
        let mut grown = vec![false];
        let mut pending = std::collections::VecDeque::new();
        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 3.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 10,
            growing_window_size_px_contours: 6,
            growing_window_size_px_attractions: 6,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };
        let outcome = grow_one_step(
            FlyingEnd {
                contour_idx: 0,
                is_start: false,
            },
            &mut pending,
            &mut contours,
            &mut point_definers,
            &mut grown,
            &mut raster,
            &config,
            GrowingPhase::SeekingOutOfBound,
            &mut Vec::new(),
        );
        assert!(matches!(outcome, GrowStepOutcome::StillFlying(_)));
        let next = *contours[0].lwg.ls.0.last().unwrap();
        let (px, py) = raster.to_px(next);
        assert_eq!(
            raster.get(px, py),
            TEMPORARY_CONTOUR,
            "the just-grown segment's own new end should already read back as TEMPORARY_CONTOUR"
        );
    }

    #[test]
    fn grow_one_step_records_the_four_push_pull_contributions_separately() {
        // Same east-heading setup as
        // `growing_step_is_deflected_by_another_flying_ends_temporary_tail`,
        // run twice, with and without one other Flying End's own temporary
        // tail nearby (repulsion, above and ahead of the straight path) --
        // isolates that one extra hit's own effect on `other_contours`
        // (it must not leak into any other term), and, unlike that other
        // test, checks the recorded breakdown itself rather than just the
        // resulting node.
        fn forces_for(mark_temporary_pixel: bool) -> GrowingStepForces {
            let ls = LineString::new(vec![c(10.5, 50.5), c(16.5, 50.5)]);
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
            raster.write_contour(0, &ls);
            let mut interior = Vec::new();
            for y in 1..(raster.height as i64 - 1) {
                for x in 1..(raster.width as i64 - 1) {
                    interior.push((x, y));
                }
            }
            raster.commit_flood_pixels(&interior);
            raster.compute_out_of_bound();
            if mark_temporary_pixel {
                // Some other Flying End's own last grow step, landing just
                // ahead and one pixel above this one's straight-line path.
                raster.mark_temporary_step(c(19.5, 51.5), c(20.0, 51.9));
            }

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let config = Config {
                bezier_linearization_step: 0.1,
                contours_step: 3.0,
                rasterization_px_size: 1.0,
                heavy_object_width: 1.0,
                heavy_object_growing: 0.2,
                circumference_fitting_points_number: 4,
                slope_lines_contours_search_radius: 3.0,
                rain_drop_step: 0.25,
                sources_per_contour_segment: 3,
                rain_drop_starting_voting_hysteresis: 3,
                undefined_gravity_vote_threshold: 0.8,
                growing_oob_seeking_max_steps: 10,
                growing_window_size_px_contours: 6,
                growing_window_size_px_attractions: 6,
                growing_step_length: 1.0,
                growing_previous_distance_direction_weight: 2.0,
                growing_out_of_bound_direction_weight: 1.0,
                growing_density_direction_weight: 1.0,
                growing_other_contours_direction_weight: -3.0,
                growing_visualization_push_pull_vectors_scale: 1.0,
            };
            let mut push_pull_vectors = Vec::new();
            let outcome = grow_one_step(
                FlyingEnd {
                    contour_idx: 0,
                    is_start: false,
                },
                &mut pending,
                &mut contours,
                &mut point_definers,
                &mut grown,
                &mut raster,
                &config,
                GrowingPhase::SeekingOutOfBound,
                &mut push_pull_vectors,
            );
            assert!(matches!(outcome, GrowStepOutcome::StillFlying(_)));
            assert_eq!(
                push_pull_vectors.len(),
                1,
                "exactly one case-(c) step was taken"
            );
            push_pull_vectors[0]
        }

        let without_temp = forces_for(false);
        let with_temp = forces_for(true);

        assert_eq!(without_temp.flying_end, c(16.5, 50.5));
        assert_eq!(
            without_temp.previous_direction,
            (2.0, 0.0),
            "heading due east, weighted by growing_previous_distance_direction_weight"
        );
        assert_eq!(
            without_temp.out_of_bound,
            (0.0, 0.0),
            "no out-of-bound pixel anywhere in this window"
        );
        assert_eq!(
            without_temp.density,
            (0.0, 0.0),
            "no high-density pixel anywhere in this window"
        );
        assert_ne!(
            without_temp.other_contours,
            (0.0, 0.0),
            "the growing contour's own trailing pixels, directly behind the Flying End, \
             must already contribute a (forward-pushing) repulsion term on their own"
        );

        // Adding the one extra temporary pixel must only move
        // `other_contours` -- the other three terms have nothing to do with
        // it and must come out exactly the same.
        assert_eq!(
            with_temp.previous_direction,
            without_temp.previous_direction
        );
        assert_eq!(with_temp.out_of_bound, without_temp.out_of_bound);
        assert_eq!(with_temp.density, without_temp.density);
        assert!(
            with_temp.other_contours.1 < without_temp.other_contours.1 - 0.05,
            "a temporary pixel sitting above the straight path must push the y component \
             further negative than the contour's own (symmetric, y=0) trailing pixels alone \
             do: without={:?} with={:?}",
            without_temp.other_contours,
            with_temp.other_contours
        );

        assert_eq!(
            with_temp.total(),
            (
                with_temp.previous_direction.0
                    + with_temp.out_of_bound.0
                    + with_temp.density.0
                    + with_temp.other_contours.0,
                with_temp.previous_direction.1
                    + with_temp.out_of_bound.1
                    + with_temp.density.1
                    + with_temp.other_contours.1
            )
        );
    }

    #[test]
    fn growing_window_hits_excludes_the_center_pixel_and_its_8_neighbors_from_contour_repulsion() {
        // Directly on `growing_window_hits`, not through `grow_one_step`:
        // three TEMPORARY_CONTOUR pixels at Chebyshev distance 0 (the
        // center itself), 1 (an immediate neighbor), and 2 from
        // `center_px` -- only the one at distance 2 should come back, even
        // with a contour window generous enough (`half_contours = 5`) to
        // reach all three.
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 20);
        let center = raster.pixel_center(10, 10);
        let neighbor = raster.pixel_center(11, 10);
        let farther = raster.pixel_center(12, 10);
        raster.mark_temporary_step(center, center);
        raster.mark_temporary_step(neighbor, neighbor);
        raster.mark_temporary_step(farther, farther);

        let hits = growing_window_hits(&raster, (10, 10), 5, 0);
        let contour_hits: Vec<Coord<f64>> = hits
            .iter()
            .filter(|(kind, _)| matches!(kind, WindowPixelKind::Contour))
            .map(|(_, center)| *center)
            .collect();

        assert_eq!(
            contour_hits,
            vec![farther],
            "only the pixel at Chebyshev distance 2 should count -- the center pixel and its \
             8 neighbors must be excluded from contour repulsion entirely: {contour_hits:?}"
        );
    }

    #[test]
    fn growing_window_size_px_contours_controls_how_far_the_repulsion_window_reaches() {
        // Same east-heading fixture again, this time with the one extra
        // temporary pixel placed 7m straight ahead (well past
        // contours_step's own 3m) -- a `growing_window_size_px_contours` of
        // 4 (half = 2px at this raster's 1m pixels) must miss it entirely,
        // while a wider one of 20 (half = 10px) must not, even though
        // `contours_step`, `rasterization_px_size`, and
        // `growing_window_size_px_attractions` are all unchanged between the
        // two: only the contour-repulsion window is under test here.
        fn other_contours_for(window_px_contours: u64, mark_far_pixel: bool) -> (f64, f64) {
            let ls = LineString::new(vec![c(10.5, 50.5), c(16.5, 50.5)]);
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
            raster.write_contour(0, &ls);
            let mut interior = Vec::new();
            for y in 1..(raster.height as i64 - 1) {
                for x in 1..(raster.width as i64 - 1) {
                    interior.push((x, y));
                }
            }
            raster.commit_flood_pixels(&interior);
            raster.compute_out_of_bound();
            if mark_far_pixel {
                // 7m straight ahead of the Flying End at (16.5, 50.5).
                raster.mark_temporary_step(c(23.0, 50.5), c(23.5, 50.5));
            }

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let config = Config {
                bezier_linearization_step: 0.1,
                contours_step: 3.0,
                rasterization_px_size: 1.0,
                heavy_object_width: 1.0,
                heavy_object_growing: 0.2,
                circumference_fitting_points_number: 4,
                slope_lines_contours_search_radius: 3.0,
                rain_drop_step: 0.25,
                sources_per_contour_segment: 3,
                rain_drop_starting_voting_hysteresis: 3,
                undefined_gravity_vote_threshold: 0.8,
                growing_oob_seeking_max_steps: 10,
                growing_window_size_px_contours: window_px_contours,
                growing_window_size_px_attractions: 4,
                growing_step_length: 1.0,
                growing_previous_distance_direction_weight: 2.0,
                growing_out_of_bound_direction_weight: 1.0,
                growing_density_direction_weight: 1.0,
                growing_other_contours_direction_weight: -3.0,
                growing_visualization_push_pull_vectors_scale: 1.0,
            };
            let mut push_pull_vectors = Vec::new();
            let outcome = grow_one_step(
                FlyingEnd {
                    contour_idx: 0,
                    is_start: false,
                },
                &mut pending,
                &mut contours,
                &mut point_definers,
                &mut grown,
                &mut raster,
                &config,
                GrowingPhase::SeekingOutOfBound,
                &mut push_pull_vectors,
            );
            assert!(matches!(outcome, GrowStepOutcome::StillFlying(_)));
            push_pull_vectors[0].other_contours
        }

        // At each window size, compare with vs without the far pixel, so
        // widening the window is the only thing that changes between the
        // two comparisons -- comparing across window sizes directly would
        // also mix in how much of the contour's own (always-visible)
        // trailing pixels each window happens to see.
        assert_eq!(
            other_contours_for(4, true),
            other_contours_for(4, false),
            "a pixel 7m away must be invisible to a growing_window_size_px_contours of 4 \
             (half = 2px)"
        );
        assert_ne!(
            other_contours_for(20, true),
            other_contours_for(20, false),
            "the same pixel must be seen once growing_window_size_px_contours is widened to 20 \
             (half = 10px)"
        );
    }

    #[test]
    fn growing_window_size_px_attractions_controls_how_far_the_matching_window_reaches() {
        // Two Flying Ends (each its own contour) 7m apart -- far past
        // contours_step's own 3m -- during the Matching phase, where case
        // (a) checks for another pending Flying End inside the attraction
        // window. A `growing_window_size_px_attractions` of 4 (half = 2px)
        // must not match them; one of 20 (half = 10px) must, even with
        // `growing_window_size_px_contours` held fixed throughout.
        fn resolves_by_matching(window_px_attractions: u64) -> bool {
            let ls_a = LineString::new(vec![c(0.5, 50.5), c(6.5, 50.5)]);
            let ls_b = LineString::new(vec![c(20.5, 50.5), c(13.5, 50.5)]);
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
            raster.write_contour(0, &ls_a);
            raster.write_contour(1, &ls_b);
            let mut interior = Vec::new();
            for y in 1..(raster.height as i64 - 1) {
                for x in 1..(raster.width as i64 - 1) {
                    interior.push((x, y));
                }
            }
            raster.commit_flood_pixels(&interior);
            raster.compute_out_of_bound();

            let mut contours = vec![
                Contour {
                    lwg: LineWithGravity::new(ls_a),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_b),
                    elevation_height: None,
                },
            ];
            let mut point_definers = Vec::new();
            let mut grown = vec![false, false];
            let end_a = FlyingEnd {
                contour_idx: 0,
                is_start: false,
            };
            let end_b = FlyingEnd {
                contour_idx: 1,
                is_start: false,
            };
            let mut pending = std::collections::VecDeque::from([end_b]);
            let config = Config {
                bezier_linearization_step: 0.1,
                contours_step: 3.0,
                rasterization_px_size: 1.0,
                heavy_object_width: 1.0,
                heavy_object_growing: 0.2,
                circumference_fitting_points_number: 4,
                slope_lines_contours_search_radius: 3.0,
                rain_drop_step: 0.25,
                sources_per_contour_segment: 3,
                rain_drop_starting_voting_hysteresis: 3,
                undefined_gravity_vote_threshold: 0.8,
                growing_oob_seeking_max_steps: 0,
                growing_window_size_px_contours: 4,
                growing_window_size_px_attractions: window_px_attractions,
                growing_step_length: 1.0,
                growing_previous_distance_direction_weight: 1.0,
                growing_out_of_bound_direction_weight: 1.0,
                growing_density_direction_weight: 1.0,
                growing_other_contours_direction_weight: -1.0,
                growing_visualization_push_pull_vectors_scale: 1.0,
            };
            let mut push_pull_vectors = Vec::new();
            let outcome = grow_one_step(
                end_a,
                &mut pending,
                &mut contours,
                &mut point_definers,
                &mut grown,
                &mut raster,
                &config,
                GrowingPhase::MatchingEnds,
                &mut push_pull_vectors,
            );
            matches!(outcome, GrowStepOutcome::Resolved)
        }

        assert!(
            !resolves_by_matching(4),
            "two Flying Ends 7m apart must not match through a growing_window_size_px_attractions \
             of 4 (half = 2px)"
        );
        assert!(
            resolves_by_matching(20),
            "the same two Flying Ends must match once growing_window_size_px_attractions is \
             widened to 20 (half = 10px)"
        );
    }

    #[test]
    fn growing_step_length_scales_the_case_c_step_distance() {
        // Straight, empty stretch (no out-of-bound/high-density/other-contour
        // pixel anywhere in the window) -- the Flying End just continues
        // along `previous_direction`, `growing_step_length * contours_step`
        // at a time.
        fn step_distance(growing_step_length: f64) -> f64 {
            let ls = LineString::new(vec![c(10.5, 50.5), c(16.5, 50.5)]);
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
            raster.write_contour(0, &ls);
            let mut interior = Vec::new();
            for y in 1..(raster.height as i64 - 1) {
                for x in 1..(raster.width as i64 - 1) {
                    interior.push((x, y));
                }
            }
            raster.commit_flood_pixels(&interior);
            raster.compute_out_of_bound();

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let config = Config {
                bezier_linearization_step: 0.1,
                contours_step: 3.0,
                rasterization_px_size: 1.0,
                heavy_object_width: 1.0,
                heavy_object_growing: 0.2,
                circumference_fitting_points_number: 4,
                slope_lines_contours_search_radius: 3.0,
                rain_drop_step: 0.25,
                sources_per_contour_segment: 3,
                rain_drop_starting_voting_hysteresis: 3,
                undefined_gravity_vote_threshold: 0.8,
                growing_oob_seeking_max_steps: 10,
                growing_window_size_px_contours: 1,
                growing_window_size_px_attractions: 1,
                growing_step_length,
                growing_previous_distance_direction_weight: 1.0,
                growing_out_of_bound_direction_weight: 1.0,
                growing_density_direction_weight: 1.0,
                growing_other_contours_direction_weight: -1.0,
                growing_visualization_push_pull_vectors_scale: 1.0,
            };
            let mut push_pull_vectors = Vec::new();
            let outcome = grow_one_step(
                FlyingEnd {
                    contour_idx: 0,
                    is_start: false,
                },
                &mut pending,
                &mut contours,
                &mut point_definers,
                &mut grown,
                &mut raster,
                &config,
                GrowingPhase::SeekingOutOfBound,
                &mut push_pull_vectors,
            );
            let next = match outcome {
                GrowStepOutcome::StillFlying(_) => *contours[0].lwg.ls.0.last().unwrap(),
                GrowStepOutcome::Resolved => panic!("expected it to still be flying"),
            };
            (next.x - 16.5).hypot(next.y - 50.5)
        }

        assert!(
            (step_distance(1.0) - 3.0).abs() < 1e-9,
            "growing_step_length of 1.0 must move the full contours_step"
        );
        assert!(
            (step_distance(0.5) - 1.5).abs() < 1e-9,
            "growing_step_length of 0.5 must move half of contours_step"
        );
    }

    #[test]
    fn growing_step_length_scales_the_case_b_snap_distance_too() {
        // An out-of-bound border pixel sitting exactly 3m ahead of the
        // Flying End, with contours_step = 4.0: closer than
        // growing_step_length(1.0) * contours_step = 4.0, so case (b) snaps
        // onto it directly, but *not* closer than
        // growing_step_length(0.5) * contours_step = 2.0, so with the
        // smaller step length it must fall through to case (c) instead and
        // still be flying afterward -- landing at (18.5, 50.5) (a pixel
        // *center*, comfortably short of the border column, rather than
        // exactly on a pixel edge as a step of 1.5 from x = 16.5 would,
        // which is why 4.0/0.5 rather than the more obvious 3.0/0.5 is used
        // here).
        fn outcome_for(growing_step_length: f64) -> GrowStepOutcome {
            let ls = LineString::new(vec![c(10.5, 50.5), c(16.5, 50.5)]);
            // Right border at pixel column 19 (width 20): its own pixel
            // center, (19.5, 50.5), sits exactly 3m from the Flying End at
            // (16.5, 50.5).
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 60);
            raster.write_contour(0, &ls);
            let mut interior = Vec::new();
            for y in 1..(raster.height as i64 - 1) {
                for x in 1..(raster.width as i64 - 1) {
                    interior.push((x, y));
                }
            }
            raster.commit_flood_pixels(&interior);
            raster.compute_out_of_bound();

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let config = Config {
                bezier_linearization_step: 0.1,
                contours_step: 4.0,
                rasterization_px_size: 1.0,
                heavy_object_width: 1.0,
                heavy_object_growing: 0.2,
                circumference_fitting_points_number: 4,
                slope_lines_contours_search_radius: 3.0,
                rain_drop_step: 0.25,
                sources_per_contour_segment: 3,
                rain_drop_starting_voting_hysteresis: 3,
                undefined_gravity_vote_threshold: 0.8,
                growing_oob_seeking_max_steps: 10,
                growing_window_size_px_contours: 6,
                growing_window_size_px_attractions: 8,
                growing_step_length,
                growing_previous_distance_direction_weight: 1.0,
                growing_out_of_bound_direction_weight: 1.0,
                growing_density_direction_weight: 1.0,
                growing_other_contours_direction_weight: -1.0,
                growing_visualization_push_pull_vectors_scale: 1.0,
            };
            let mut push_pull_vectors = Vec::new();
            grow_one_step(
                FlyingEnd {
                    contour_idx: 0,
                    is_start: false,
                },
                &mut pending,
                &mut contours,
                &mut point_definers,
                &mut grown,
                &mut raster,
                &config,
                GrowingPhase::SeekingOutOfBound,
                &mut push_pull_vectors,
            )
        }

        assert!(
            matches!(outcome_for(1.0), GrowStepOutcome::Resolved),
            "3m is closer than 1.0 * 4.0 = 4.0m: case (b) should snap onto the border directly"
        );
        assert!(
            matches!(outcome_for(0.5), GrowStepOutcome::StillFlying(_)),
            "3m is not closer than 0.5 * 4.0 = 2.0m: case (b) should not trigger, leaving it to \
             case (c) instead"
        );
    }

    #[test]
    fn growing_raster_matches_the_final_ls_even_after_several_steps_then_closing() {
        // A tilted "C": both ends start well outside each other's growing
        // window, angled slightly inward, so each takes several case-(c)
        // steps (moving in a straight line -- nothing else is on this map
        // to react to) before finally entering the other's window and
        // closing. Closing re-samples the *whole* ring against a
        // perimeter-adjusted step (Appendix 1), which does not, in general,
        // land back on the exact intermediate points each step produced --
        // so if those intermediate steps had each written themselves into
        // the Contour Raster as they were grown, stale pixels from before
        // that final shift would be left behind. They must not be: nothing
        // gets written until each end's own final shape is known.
        let ls = LineString::new(vec![
            c(12.0, 10.0),
            c(10.0, 20.0),
            c(20.0, 20.0),
            c(18.0, 10.0),
        ]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);
        raster.compute_out_of_bound();
        let contour = Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
        };

        let mut result = Step1Result {
            contours: vec![contour],
            raw_polylines: vec![Vec::new()],
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            grown_by_growing_process: vec![false],
            growing_push_pull_vectors: Vec::new(),
            warnings: Vec::new(),
        };
        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 5.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            growing_oob_seeking_max_steps: 0,
            growing_window_size_px_contours: 10,
            growing_window_size_px_attractions: 10,
            growing_step_length: 1.0,
            growing_previous_distance_direction_weight: 1.0,
            growing_out_of_bound_direction_weight: 1.0,
            growing_density_direction_weight: 1.0,
            growing_other_contours_direction_weight: -1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };

        let warnings = run_growing(&mut result, &config);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(result.contours.len(), 1);

        // Every pixel cleanly marked as contour 0 in the grown raster (not
        // conflicted into high density by the original body, out-of-bound,
        // or anything else already there) must also be touched by a fresh
        // draw of the *final* `ls` alone -- otherwise it is a stale pixel
        // left over from an intermediate, pre-resample position that was
        // written and then abandoned once resampling moved on.
        let contour_0 = crate::contour_raster::CONTOUR_0_MATRIX_VALUE;
        let mut expected = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        expected.write_contour(0, &result.contours[0].lwg.ls);
        let mut any_contour_pixel = false;
        for y in 0..40i64 {
            for x in 0..40i64 {
                if result.raster.get(x, y) == contour_0 {
                    any_contour_pixel = true;
                    assert_eq!(
                        expected.get(x, y),
                        contour_0,
                        "pixel ({x},{y}) is marked contour 0 in the grown raster but isn't \
                         on the final ls's own path -- a stale pixel from an intermediate, \
                         pre-resample position"
                    );
                }
            }
        }
        assert!(any_contour_pixel, "expected at least one contour pixel");
    }
}
