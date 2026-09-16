//! Step 1 of `Contours-to-Raster.md`: pulling Contours, Slope Lines, Jumps
//! and Heavy Objects out of a parsed map, building the Contour Raster, and
//! turning the supporting symbols into gravity evidence.

use geo::{Coord, LineString, Polygon};

use crate::contour_geometry::{self, coords_to_linestrings, nearest_index, resample_equal_chords};
use crate::contour_raster::{
    ContourRaster, PixelWalkStep, StepHit, StepWalkOutcome, CONTOUR_0_MATRIX_VALUE, HIGH_DENSITY,
    OUT_OF_BOUND, TEMPORARY_CONTOUR,
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
    /// regardless of whether it will end up finding any contour inside it --
    /// kept for the `--create_svg` visualization, the same way a Jump's
    /// polygon is drawn (see `LineGravityDefiners::poly`), so the actual
    /// search area a `heavy_object_width`/`heavy_object_growing` choice
    /// produces can be judged by eye against the real pixels, *and*
    /// re-scanned by [`resolve_heavy_object_gravity`] once the Growing
    /// Process has finished, to find each polygon's own intersecting
    /// contour(s) against their final geometry.
    pub heavy_object_polygons: Vec<Polygon<f64>>,
    /// Every Flying End's own position *before* the Growing Process ran --
    /// kept only for the `--create_svg` visualization's red rings (see
    /// `run_growing_process`).
    pub pre_growing_flying_ends: Vec<Coord<f64>>,
    /// Parallel to `contours`: whether that contour was touched by the
    /// Growing Process (grown, merged into, or both, in any of its three
    /// passes) -- kept only for `--create_svg`'s numbered files after
    /// `00_..._step1.svg` (`01_..._step1_close_search.svg`,
    /// `02_..._step1_growing_seeking.svg`,
    /// `03_..._step1_growing_matching.svg`), each of which draws these in
    /// blue instead of green.
    pub grown_by_growing_process: Vec<bool>,
    /// One entry per integration step actually taken (see
    /// `grow_one_step`), empty until `run_growing_seeking`/
    /// `run_growing_matching` run -- Close Search takes no integration
    /// steps, so this is always empty right after it. Kept only for
    /// `--create_svg`'s `02_..._step1_growing_seeking.svg`/
    /// `03_..._step1_growing_matching.svg`, which draw each step's own four
    /// force contributions as separate colored vectors
    /// (`growing_visualization_push_pull_vectors_scale`-scaled).
    pub growing_push_pull_vectors: Vec<GrowingStepForces>,
    /// Every integration step's own resulting position (Step 1's Growing
    /// Process's Seeking/Matching passes) -- one entry per step actually
    /// taken (an ordinary still-flying position, or a tunneling-safe
    /// landing position), empty until `run_growing_seeking`/
    /// `run_growing_matching` run. A merge/close (in any pass, including
    /// Close Search) produces no integration step and so contributes no
    /// entry. Kept only for `--create_svg`'s
    /// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`,
    /// which draw each as a small dot.
    pub growing_integration_step_dots: Vec<Coord<f64>>,
    /// Recoverable problems found along the way.
    pub warnings: Vec<String>,
}

/// One integration step's own flying-end position (the vectors' shared
/// tail, *before* the step) and the four separate force contributions
/// [`growing_forces`] computes there ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)),
/// before they are summed into the step's actual net force
/// ([`GrowingStepForces::total`]) -- kept only for `--create_svg`'s
/// `02_..._step1_growing_seeking.svg`/`03_..._step1_growing_matching.svg`
/// visualization.
#[derive(Clone, Copy, Debug)]
pub struct GrowingStepForces {
    /// The Flying End's own position before this step.
    pub flying_end: Coord<f64>,
    /// The contour-pixel potential-well force's own contribution, summed
    /// over every qualifying pixel in `contour_force_window` (Appendix 5).
    pub contour: (f64, f64),
    /// `out_of_bound_force`'s own contribution -- always `(0.0, 0.0)` while
    /// `phase` is `MatchingEnds`, since the term is dropped entirely then
    /// (see `growing_forces`).
    pub out_of_bound: (f64, f64),
    /// `density_region_force`'s own contribution.
    pub density: (f64, f64),
    /// `flying_end_force`'s own contribution -- always `(0.0, 0.0)` while
    /// `phase` is `SeekingOutOfBound`, since matching (and so this term) is
    /// disabled entirely then (see `growing_forces`).
    pub flying_end_force: (f64, f64),
}

impl GrowingStepForces {
    /// The Flying End's actual net force this step, used directly as its
    /// velocity (no mass, no inertia): the four contributions summed.
    fn total(&self) -> (f64, f64) {
        (
            self.contour.0 + self.out_of_bound.0 + self.density.0 + self.flying_end_force.0,
            self.contour.1 + self.out_of_bound.1 + self.density.1 + self.flying_end_force.1,
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
                    //
                    // Unlike a Jump, this polygon is not scanned for
                    // intersecting contours here: a Heavy Object's own
                    // gravity fit is deferred until
                    // `resolve_heavy_object_gravity` runs, after the Growing
                    // Process has finished merging/closing every contour --
                    // see that function's own doc comment for why.
                    let poly = contour_geometry::ls_to_polygon(
                        ls,
                        config.heavy_object_width,
                        config.heavy_object_growing,
                    );
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
        growing_integration_step_dots: Vec::new(),
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

/// The unit vector a Flying End points along, continuing past its own tip
/// (the direction Close Search's cone opens toward, below): `ls[1] ->
/// ls[0]` for `is_start` (the contour's own first segment, extrapolated
/// backward past its start), `ls[second-to-last] -> ls[last]` for the other
/// end. `None` for a degenerate (near-zero-length) last segment -- that end
/// simply can't run its own two searches (below), though it stays a valid
/// target for another end's.
fn flying_end_direction(ls: &LineString<f64>, is_start: bool) -> Option<(f64, f64)> {
    let (from, to) = if is_start {
        (ls.0[1], ls.0[0])
    } else {
        let n = ls.0.len();
        (ls.0[n - 2], ls.0[n - 1])
    };
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        None
    } else {
        Some((dx / len, dy / len))
    }
}

/// `Some(distance)` if `to` lies within `max_distance` of `from` and inside
/// the cone opening along `dir`, half `cos_half_fov`'s own angle to each
/// side (Step 1's Growing Process, Close Search) -- `None` otherwise, or if
/// `to` coincides with `from`. `cos_half_fov` is
/// `(searching_fov.to_radians() / 2.0).cos()`, computed once by the caller.
fn in_search_cone(
    from: Coord<f64>,
    dir: (f64, f64),
    to: Coord<f64>,
    max_distance: f64,
    cos_half_fov: f64,
) -> Option<f64> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let distance = dx.hypot(dy);
    if distance < 1e-9 || distance > max_distance {
        return None;
    }
    let cos_theta = (dir.0 * dx + dir.1 * dy) / distance;
    if cos_theta < cos_half_fov {
        return None;
    }
    Some(distance)
}

/// Whether Close Search's straight segment `from -> to` (`from` on
/// `from_contour`, `to` on `to_contour`) is a valid candidate connection
/// between two Flying Ends ([`ContourRaster::first_hit_along_step`],
/// Appendix 4): only a *different* contour in the way blocks it, matching
/// this search's own job of telling apart "these two ends genuinely belong
/// together" from "something else is physically between them." An
/// out-of-bound pixel crossed along the way does not block it -- the empty/
/// unclaimed area directly between two close Flying Ends is entirely
/// expected and has nothing to do with another contour being there; reaching
/// `to_contour`'s own body is exactly what reaching `to` is supposed to do,
/// not a genuine obstruction either. High-density does block it, since it
/// represents unresolved contour conflict (two or more contours, or a Jump's
/// polygon), not open space.
fn end_connection_clear(
    raster: &ContourRaster,
    from: Coord<f64>,
    to: Coord<f64>,
    from_contour: usize,
    to_contour: usize,
) -> bool {
    match raster.first_hit_along_step(from, to, from_contour as u64) {
        None | Some(StepHit::OutOfBound) => true,
        Some(StepHit::Contour(idx)) => idx as usize == to_contour,
        Some(StepHit::HighDensity) => false,
    }
}

/// Whether Close Search's straight segment `from -> to` (`from` on
/// `from_contour`) is a valid candidate connection to an out-of-bound pixel:
/// only once it actually reaches out-of-bound territory without crossing a
/// contour or high-density pixel first.
fn end_to_out_of_bound_clear(
    raster: &ContourRaster,
    from: Coord<f64>,
    to: Coord<f64>,
    from_contour: usize,
) -> bool {
    matches!(
        raster.first_hit_along_step(from, to, from_contour as u64),
        Some(StepHit::OutOfBound)
    )
}

/// Close Search's own first search (Step 1's Growing Process, before Seeking
/// ever runs): among every *other* still-pending Flying End within `pos`'s
/// own cone (`dir`/`cos_half_fov`/`max_distance`), the index into `pending`
/// of the closest one whose straight connection back to `pos` is valid
/// ([`end_connection_clear`]) -- `None` if none qualify. Candidates are
/// sorted by distance first and the crossing check tried in that order, so
/// the first one found valid is also the closest one.
#[allow(clippy::too_many_arguments)]
fn nearest_valid_end_candidate(
    pos: Coord<f64>,
    dir: (f64, f64),
    cos_half_fov: f64,
    max_distance: f64,
    contour_idx: usize,
    pending: &std::collections::VecDeque<FlyingEnd>,
    contours: &[Contour],
    raster: &ContourRaster,
) -> Option<usize> {
    let mut candidates: Vec<(f64, usize)> = pending
        .iter()
        .enumerate()
        .filter_map(|(i, other)| {
            let other_pos =
                flying_end_position(&contours[other.contour_idx].lwg.ls, other.is_start);
            in_search_cone(pos, dir, other_pos, max_distance, cos_half_fov).map(|d| (d, i))
        })
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    candidates
        .into_iter()
        .find(|&(_, i)| {
            let other = pending[i];
            let other_pos =
                flying_end_position(&contours[other.contour_idx].lwg.ls, other.is_start);
            end_connection_clear(raster, pos, other_pos, contour_idx, other.contour_idx)
        })
        .map(|(_, i)| i)
}

/// Close Search's own second search (Step 1's Growing Process), only tried
/// once [`nearest_valid_end_candidate`] found nothing: among every
/// `OUT_OF_BOUND` pixel within `pos`'s own cone, the world-space center of
/// the closest one whose straight connection back to `pos` is valid
/// ([`end_to_out_of_bound_clear`]) -- `None` if none qualify. Scans the
/// pixel-space bounding box of `pos ± max_distance`, the same box-then-
/// filter shape as [`ContourRaster::nearest_contour_within_radius`]/
/// [`growing_window_hits`], sorting candidates by distance the same way
/// [`nearest_valid_end_candidate`] does.
fn nearest_valid_ob_pixel(
    pos: Coord<f64>,
    dir: (f64, f64),
    cos_half_fov: f64,
    max_distance: f64,
    contour_idx: usize,
    raster: &ContourRaster,
) -> Option<Coord<f64>> {
    let (px_min_x, px_min_y) = raster.to_px(Coord {
        x: pos.x - max_distance,
        y: pos.y - max_distance,
    });
    let (px_max_x, px_max_y) = raster.to_px(Coord {
        x: pos.x + max_distance,
        y: pos.y + max_distance,
    });
    let mut candidates: Vec<(f64, Coord<f64>)> = Vec::new();
    for y in px_min_y..=px_max_y {
        for x in px_min_x..=px_max_x {
            if raster.get(x, y) != OUT_OF_BOUND {
                continue;
            }
            let center = raster.pixel_center(x, y);
            if let Some(d) = in_search_cone(pos, dir, center, max_distance, cos_half_fov) {
                candidates.push((d, center));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    candidates
        .into_iter()
        .find(|&(_, center)| end_to_out_of_bound_clear(raster, pos, center, contour_idx))
        .map(|(_, center)| center)
}

/// Step 1's Growing Process (see the doc), the preliminary Close Search pass
/// that runs before Seeking ever takes an integration step: a cheap,
/// non-iterative geometric check rather than a physics simulation. Builds
/// the same initial Flying Ends list Seeking itself would
/// ([`collect_flying_ends`]) and gives each exactly one turn, in order, as
/// the active searcher -- but every end, tried or not, stays available the
/// whole time as a candidate for every *other* end's own turn (a still-
/// untried end can be consumed by an earlier end's own search; a
/// once-tried, still-unresolved end can likewise still be found and
/// resolved by a later one's).
///
/// Each turn: [`nearest_valid_end_candidate`] first, and if that finds
/// nothing, [`nearest_valid_ob_pixel`]. A Flying-End match closes this
/// contour into a ring ([`close_contour`]) if the candidate is this same
/// contour's own other end, or splices the two contours together
/// ([`merge_contours`]) otherwise -- resolving both ends at once, exactly
/// like Matching's own merge check. An out-of-bound match resolves this one
/// end directly onto it, the same append-resample-redraw tail an ordinary
/// integration step's own landing uses ([`finalize_contour_ls`]). Either
/// way that Flying End skips both Seeking and Matching entirely; finding
/// neither leaves it untouched, to be picked up again by
/// [`run_growing_seeking`]'s own fresh [`collect_flying_ends`] call.
///
/// Unlike Seeking/Matching, nothing here is a partial, still-flying step --
/// every resolution is atomic, so no `TEMPORARY_CONTOUR` bookkeeping is
/// needed.
pub fn run_growing_close_search(result: &mut Step1Result, config: &Config) {
    let mut grown = std::mem::take(&mut result.grown_by_growing_process);
    let mut pending: std::collections::VecDeque<FlyingEnd> =
        collect_flying_ends(&result.contours, &result.raster).into();
    let cos_half_fov = (config.searching_fov.to_radians() / 2.0).cos();
    let turns = pending.len();

    for _ in 0..turns {
        let Some(end) = pending.pop_front() else {
            break;
        };
        let pos = flying_end_position(&result.contours[end.contour_idx].lwg.ls, end.is_start);
        let Some(dir) =
            flying_end_direction(&result.contours[end.contour_idx].lwg.ls, end.is_start)
        else {
            pending.push_back(end);
            continue;
        };

        if let Some(i) = nearest_valid_end_candidate(
            pos,
            dir,
            cos_half_fov,
            config.searching_distance,
            end.contour_idx,
            &pending,
            &result.contours,
            &result.raster,
        ) {
            let other = pending.remove(i).expect("index just found by search");
            if other.contour_idx == end.contour_idx {
                close_contour(
                    end.contour_idx,
                    end.is_start,
                    &mut result.contours,
                    &mut result.raster,
                    config,
                    &mut grown,
                );
            } else {
                merge_contours(
                    end.contour_idx,
                    end.is_start,
                    other.contour_idx,
                    other.is_start,
                    &mut result.contours,
                    &mut result.point_definers,
                    &mut pending,
                    &mut grown,
                    &mut result.raster,
                    config,
                );
            }
            continue;
        }

        if let Some(center) = nearest_valid_ob_pixel(
            pos,
            dir,
            cos_half_fov,
            config.searching_distance,
            end.contour_idx,
            &result.raster,
        ) {
            append_node(&mut result.contours, end.contour_idx, end.is_start, center);
            finalize_contour_ls(
                end.contour_idx,
                &mut result.contours,
                &mut result.raster,
                config,
                &mut grown,
            );
            continue;
        }

        pending.push_back(end);
    }

    result.grown_by_growing_process = grown;
}

enum WindowPixelKind {
    OutOfBound,
    HighDensity,
    Contour,
}

/// Every out-of-bound, high-density, or contour pixel's world-space center
/// found around `flying_end` (Appendix 5), each kind checked against its own
/// circular window, in ground meters: a `Contour` pixel (a real one, or a
/// `TEMPORARY_CONTOUR` tail -- some Flying End's own not-yet-final tail, see
/// [`ContourRaster::walk_growing_integration_step`], counts as one here too,
/// so two Flying Ends growing at the same time repel each other's tails
/// instead of only reacting to already-finalized contours) only counts
/// within `contour_force_window` meters of `flying_end` -- its own pixel and
/// its 8 immediate neighbors (grid adjacency, not distance) are always
/// excluded, since the Flying End always sits right on top of its own
/// just-written body there, and that self-proximity would otherwise swamp
/// the force with a huge, meaningless push instead of reflecting genuinely
/// nearby contour pixels. An `OutOfBound`/`HighDensity` pixel only counts
/// within `attraction_force_window` meters, no such exclusion. Both windows
/// are scanned in one pass, over their shared (larger) pixel-space bounding
/// box, each pixel then kept or dropped by its own kind's own radius (true
/// Euclidean distance from `flying_end`'s own continuous position, not the
/// pixel grid).
fn growing_window_hits(
    raster: &ContourRaster,
    flying_end: Coord<f64>,
    center_px: (i64, i64),
    config: &Config,
) -> Vec<(WindowPixelKind, Coord<f64>)> {
    let half_contours_px = (config.contour_force_window / raster.px_size).ceil() as i64;
    let half_attractions_px = (config.attraction_force_window / raster.px_size).ceil() as i64;
    let half_px = half_contours_px.max(half_attractions_px).max(1);
    let mut hits = Vec::new();
    for y in (center_px.1 - half_px)..=(center_px.1 + half_px) {
        for x in (center_px.0 - half_px)..=(center_px.0 + half_px) {
            let val = raster.get(x, y);
            let kind = if val == OUT_OF_BOUND {
                WindowPixelKind::OutOfBound
            } else if val == HIGH_DENSITY {
                WindowPixelKind::HighDensity
            } else if val == TEMPORARY_CONTOUR || val >= CONTOUR_0_MATRIX_VALUE {
                let cheby = (x - center_px.0).abs().max((y - center_px.1).abs());
                if cheby <= 1 {
                    continue; // the Flying End's own pixel and its 8 neighbors
                }
                WindowPixelKind::Contour
            } else {
                continue;
            };
            let center = raster.pixel_center(x, y);
            let d = (center.x - flying_end.x).hypot(center.y - flying_end.y);
            let within = match kind {
                WindowPixelKind::Contour => d <= config.contour_force_window,
                WindowPixelKind::OutOfBound | WindowPixelKind::HighDensity => {
                    d <= config.attraction_force_window
                }
            };
            if within {
                hits.push((kind, center));
            }
        }
    }
    hits
}

/// The contour-pixel potential-well force's own scalar magnitude at
/// Euclidean distance `x` (ground meters) from a single contour/
/// `TEMPORARY_CONTOUR` pixel (Appendix 5): positive (repulsive) for `x`
/// between `0` and `contour_force_equilibrium` (cfe), a cubic curve from
/// `contour_force_max_repulsion` (cfmr) at `x = 0` fading to zero at `x =
/// cfe`; negative (attractive) for `x` between `cfe` and
/// `contour_force_second_equilibrium` (cfse), a smoothstep from zero at
/// `cfe` to `contour_force_max_attraction` (cfma, negative) at `cfse`; held
/// constant at `cfma` for `x > cfse`. The magnitude alone carries the sign:
/// a positive value pushes away from the pixel, a negative one pulls toward
/// it, so callers apply it along the same "away from the pixel" unit vector
/// throughout, with no separate sign branch needed.
pub(crate) fn contour_force_magnitude(x: f64, config: &Config) -> f64 {
    let cfmr = config.contour_force_max_repulsion;
    let cfe = config.contour_force_equilibrium;
    let cfma = config.contour_force_max_attraction;
    let cfse = config.contour_force_second_equilibrium;
    if x <= cfe {
        (2.0 * cfmr / cfe.powi(3)) * x.powi(3) - (3.0 * cfmr / cfe.powi(2)) * x.powi(2) + cfmr
    } else if x <= cfse {
        let t = (x - cfe) / (cfse - cfe);
        cfma * (3.0 * t * t - 2.0 * t * t * t)
    } else {
        cfma
    }
}

/// Appendix 5's real, Newton-valued forces, broken down by term rather than
/// pre-summed: the contour-pixel potential well
/// (`contour_force_magnitude`, summed over every hit within
/// `contour_force_window`, unit vector away from the pixel), and the three
/// constant-magnitude attractions (`out_of_bound_force`/
/// `density_region_force`/`flying_end_force`, each summed over every hit
/// within `attraction_force_window`, unit vector toward it). Once a Flying
/// End has moved on to `MatchingEnds` (see [`GrowingPhase`]), the
/// out-of-bound term is dropped entirely -- it's already had its dedicated,
/// matching-free budget to reach the border on its own and didn't, so no
/// longer being pulled toward one lets it settle into matching another
/// nearby Flying End instead; conversely the flying-end term is dropped
/// entirely during `SeekingOutOfBound`, since matching itself is disabled
/// then. The breakdown itself (rather than just [`GrowingStepForces::total`])
/// is kept only so `grow_one_step` can record it into
/// `Step1Result::growing_push_pull_vectors` for `--create_svg`'s benefit --
/// the Growing Process itself only ever needs the summed net force.
fn growing_forces(
    flying_end: Coord<f64>,
    hits: &[(WindowPixelKind, Coord<f64>)],
    other_ends: &[Coord<f64>],
    config: &Config,
    phase: GrowingPhase,
) -> GrowingStepForces {
    let mut forces = GrowingStepForces {
        flying_end,
        contour: (0.0, 0.0),
        out_of_bound: (0.0, 0.0),
        density: (0.0, 0.0),
        flying_end_force: (0.0, 0.0),
    };
    for (kind, center) in hits {
        // Each of the raster-derived terms now belongs to exactly one
        // phase: `Contour`/`OutOfBound` to `SeekingOutOfBound` alone,
        // `HighDensity` to `MatchingEnds` alone (`flying_end_force`, the
        // fourth term, is computed separately below and is `MatchingEnds`
        // -only too).
        let dropped = match phase {
            GrowingPhase::SeekingOutOfBound => matches!(kind, WindowPixelKind::HighDensity),
            GrowingPhase::MatchingEnds => {
                matches!(kind, WindowPixelKind::OutOfBound | WindowPixelKind::Contour)
            }
        };
        if dropped {
            continue;
        }
        let (bx, by) = (center.x - flying_end.x, center.y - flying_end.y);
        let d = bx.hypot(by);
        if d < 1e-12 {
            continue; // the pixel sits exactly on the Flying End
        }
        let (ux, uy) = (bx / d, by / d);
        match kind {
            WindowPixelKind::OutOfBound => {
                forces.out_of_bound.0 += config.out_of_bound_force * ux;
                forces.out_of_bound.1 += config.out_of_bound_force * uy;
            }
            WindowPixelKind::HighDensity => {
                forces.density.0 += config.density_region_force * ux;
                forces.density.1 += config.density_region_force * uy;
            }
            WindowPixelKind::Contour => {
                let mag = contour_force_magnitude(d, config);
                // Away from the pixel, not toward it (unlike `ux`/`uy`
                // above): a positive `mag` (repulsion, close range) must
                // push the Flying End away from the contour pixel, and a
                // negative one (attraction, far range) must pull it back.
                forces.contour.0 -= mag * ux;
                forces.contour.1 -= mag * uy;
            }
        }
    }
    if phase == GrowingPhase::MatchingEnds {
        for other in other_ends {
            let (bx, by) = (other.x - flying_end.x, other.y - flying_end.y);
            let d = bx.hypot(by);
            if d < 1e-12 || d > config.attraction_force_window {
                continue;
            }
            // Cubic falloff from `flying_end_force` at zero distance to
            // zero at `attraction_force_window` -- unlike `out_of_bound_force`/
            // `density_region_force`, which stay full-strength across their
            // own window. Without any falloff at all, every pending Flying
            // End within range pulled with the same full, undamped magnitude
            // regardless of distance, so in a crowded cluster (several ends
            // converging at once) the *nearest* end's own pull was easily
            // swamped by the combined pull of several more distant ones, and
            // that combined pull's own direction could swing sharply from
            // one integration step to the next as other ends resolved out of
            // range -- producing a sharp, physically implausible kink right
            // where two contours finally merge (see `merge_contours`), which
            // then reads as a genuine gravity conflict in Step 2 even though
            // the two readings around it agree about which side is downhill.
            // A *linear* falloff (`1 - d / window`) fixed that, but spends
            // most of the window at a small fraction of `flying_end_force`,
            // so two ends starting out near the far edge of each other's
            // window crawl for many integration steps before the pull ever
            // becomes meaningful. Cubing the normalized distance instead
            // keeps the pull near full strength across most of the window
            // and only rolls it off sharply right near the edge -- still
            // exactly `0` at `d = attraction_force_window`, so the boundary
            // stays continuous and the kink above doesn't come back.
            let t = d / config.attraction_force_window;
            let falloff = 1.0 - t * t * t;
            let mag = config.flying_end_force * falloff;
            forces.flying_end_force.0 += mag * bx / d;
            forces.flying_end_force.1 += mag * by / d;
        }
    }
    forces
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
    finalize_contour_ls(contour_idx, contours, raster, config, grown);
}

/// The shared tail of every place a Flying End resolves by appending one
/// final node to its own contour's `ls` (a close, an ordinary integration
/// step's landing, or Close Search's own out-of-bound landing, below): the
/// new node's own re-derivation can shift *every* node, not just the new
/// one -- including ones from the contour's own original body, already
/// drawn into the Contour Raster long before growing ever started -- so its
/// whole previous footprint is cleared first (see
/// [`ContourRaster::clear_contour`]) rather than drawing the new one
/// additively on top of the old. Callers append their own new node to
/// `contours[contour_idx].lwg.ls` themselves before calling this.
fn finalize_contour_ls(
    contour_idx: usize,
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    config: &Config,
    grown: &mut [bool],
) {
    let resampled = resample_equal_chords(&contours[contour_idx].lwg.ls, config.contours_step);
    contours[contour_idx].lwg.ls = resampled.clone();
    raster.clear_contour(contour_idx as u64);
    raster.write_contour(contour_idx as u64, &resampled);
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
    // Two Flying Ends merge as soon as they come within
    // `flying_end_merge_distance` of each other (the Matching phase's own
    // trigger), not only once they actually coincide -- so `merged_pts`'s
    // own last point (keep's join) and `tail_pts`'s own first point (tail's
    // join) can still be genuinely meters apart here, up to that same
    // distance. Dropping tail's join point unconditionally, on the
    // assumption the two are always (effectively) the same position, skips
    // that real waypoint whenever they are not: the merged path then runs
    // straight from keep's join to wherever tail's *next* point already
    // was -- heading off in tail's own, unrelated pre-merge direction --
    // putting a sharp, physically implausible kink in the merged contour
    // right there, which Step 2 then reads as a genuine gravity conflict
    // between readings on either side of it even when they agree about
    // which side is downhill. Only drop it when the two really are
    // (near-)duplicates of the same position, the case this was meant to
    // handle; otherwise keep both, so the merged path actually bridges the
    // gap between them instead of jumping over it.
    if let (Some(&keep_last), Some(&tail_first)) = (merged_pts.last(), tail_pts.first()) {
        if (keep_last.x - tail_first.x).hypot(keep_last.y - tail_first.y) < 1e-6 {
            tail_pts.remove(0);
        }
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

/// Undoes every step a Flying End took during `SeekingOutOfBound` (Phase 1)
/// once it hits its own `growing_oob_seeking_max_steps` budget without
/// resolving, so `MatchingEnds` (Phase 2, [`run_growing_matching`]) reacts to
/// the Flying End's own pre-Seeking position, not wherever the
/// (matching-blind) Seeking search happened to wander off to -- Seeking
/// exists to give a Flying End near its own border a fair, matching-free
/// shot at reaching it, not to permanently commit an unsuccessful search's
/// own path once that hasn't panned out. `original` is this end's own
/// position exactly as it was before Seeking ever touched it.
///
/// Deliberately *not* found by counting back the number of nodes
/// `grow_one_step` appended (one per `StillFlying` outcome) -- a plain count
/// would be simple, but this same contour's *other* end resolving somewhere
/// in the middle of this end's own Seeking run resamples the *whole* `ls`
/// (`resample_equal_chords`, on `grow_one_step`'s own `Landed` branch, case
/// (a)/(b) of the Growing Process), which can both change every node's own
/// position *and* the total node count -- silently invalidating a plain
/// count kept across that event. Instead, `ls`'s own node closest to
/// `original` is found by scanning it (exact if no resample intervened
/// since Seeking started for this end; otherwise the closest survivor after
/// `resample_equal_chords` redistributed points along the same curve) and
/// treated as the boundary between this end's own now-abandoned growth
/// (this end's own side of it) and everything before that (left alone). The
/// path from this end's own current position back to that boundary is
/// handed to [`ContourRaster::revert_temporary_trail`] so the
/// `TEMPORARY_CONTOUR` pixels this now-abandoned attempt marked along the
/// way don't linger to attract/repel this same Flying End as though its own
/// discarded trail were some other, separate object once it restarts
/// `MatchingEnds` from this same, now-restored position.
fn revert_seeking_growth(
    contour_idx: usize,
    is_start: bool,
    original: Coord<f64>,
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    temp_owner: &std::collections::HashMap<(i64, i64), usize>,
) {
    let ls = &mut contours[contour_idx].lwg.ls;
    let boundary = nearest_index(ls, original);
    if is_start {
        let path = ls.0[..=boundary].to_vec();
        raster.revert_temporary_trail(contour_idx, &path, temp_owner);
        ls.0.drain(..boundary);
        ls.0[0] = original;
    } else {
        let path = ls.0[boundary..].to_vec();
        raster.revert_temporary_trail(contour_idx, &path, temp_owner);
        ls.0.truncate(boundary + 1);
        let last = ls.0.len() - 1;
        ls.0[last] = original;
    }
}

/// A generous cap on the *total* number of integration steps taken across
/// every Flying End combined, scaled by how many there were to start with --
/// purely a safety valve against an unbounded loop (e.g. an oscillating
/// limit cycle between two density clusters), not part of the doc's own
/// algorithm, which assumes every path eventually reaches the out-of-bound
/// ring.
const MAX_GROWING_STEPS_PER_END: u64 = 100;

/// What one call to [`grow_one_step`] did to the Flying End it was given.
enum GrowStepOutcome {
    /// Resolved (landed on an out-of-bound/high-density pixel, or merged
    /// with another Flying End) -- nothing left to grow.
    Resolved,
    /// Still flying after this one step; here's its new position.
    StillFlying(FlyingEnd),
}

/// Advances one Flying End by exactly one integration step of the Growing
/// Process (see the doc). `pending` is every *other* Flying End still
/// waiting on its own next step (`end` itself is not in it -- the caller
/// already popped it off before calling this); ignored entirely while
/// `phase` is `SeekingOutOfBound`, since merging is disabled then.
/// `temp_owner` maps a `TEMPORARY_CONTOUR` pixel to the contour that placed
/// it, spanning the whole `run_growing` call (both phases, every Flying
/// End), so a step revisiting its own earlier trail can be told apart from
/// a genuine conflict with a different contour.
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
    dots: &mut Vec<Coord<f64>>,
    temp_owner: &mut std::collections::HashMap<(i64, i64), usize>,
    warnings: &mut Vec<String>,
) -> GrowStepOutcome {
    let (contour_idx, is_start) = (end.contour_idx, end.is_start);

    let pos = flying_end_position(&contours[contour_idx].lwg.ls, is_start);
    if !is_flying(raster, pos) {
        return GrowStepOutcome::Resolved;
    }

    // Merge check (Matching phase only): skipped outright while still
    // seeking the out-of-bound area on its own (see `GrowingPhase`) -- two
    // contours running close and parallel near the border must each reach
    // it independently, not snap onto each other just because they happen
    // to be close. Two pending Flying Ends merge the moment their real,
    // continuous Euclidean distance drops below `flying_end_merge_distance`
    // -- `flying_end_force` only pulls them together; this distance check
    // is what actually finalizes the merge.
    if phase == GrowingPhase::MatchingEnds {
        let other = pending.iter().position(|o| {
            let other_pos = flying_end_position(&contours[o.contour_idx].lwg.ls, o.is_start);
            (other_pos.x - pos.x).hypot(other_pos.y - pos.y) < config.flying_end_merge_distance
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

    // Force computation (Appendix 5): contour potential well plus, phase
    // permitting, the three constant-magnitude attractions.
    let (px, py) = raster.to_px(pos);
    let hits = growing_window_hits(raster, pos, (px, py), config);
    let other_ends: Vec<Coord<f64>> = if phase == GrowingPhase::MatchingEnds {
        pending
            .iter()
            .map(|o| flying_end_position(&contours[o.contour_idx].lwg.ls, o.is_start))
            .collect()
    } else {
        Vec::new()
    };
    let forces = growing_forces(pos, &hits, &other_ends, config, phase);
    let total = forces.total();
    push_pull_vectors.push(forces);

    // Overdamped integration: the net force *is* the velocity (no mass, no
    // inertia, nothing persisted between steps) -- displacement this step
    // is that force times `grow_time_step`. A zero net force means the
    // Flying End simply doesn't move this step (still counts toward
    // Seeking's own budget, so a Flying End with nothing pulling on it
    // doesn't loop forever without ever falling through to Matching).
    if total.0 == 0.0 && total.1 == 0.0 {
        return GrowStepOutcome::StillFlying(FlyingEnd {
            contour_idx,
            is_start,
        });
    }
    // `matching_min_force` (Matching only): a nonzero net force can still be
    // vanishingly small -- two Flying Ends near the far edge of each other's
    // `attraction_force_window` (see `flying_end_force`'s own falloff), or a
    // density/flying-end pull partly cancelling against a third nearby end
    // -- and without a floor, that crawls toward a merge over an
    // impractically large number of integration steps. Scaling the net
    // force up to `matching_min_force`'s own magnitude here, direction
    // unchanged, guarantees a step always covers at least
    // `matching_min_force * grow_time_step` meters. Seeking is left alone:
    // its own `growing_oob_seeking_max_steps` budget already bounds how
    // long a Flying End can spend there, and a weak contour-pixel pull
    // genuinely means little is nearby to react to, not something to force
    // along faster.
    let total = if phase == GrowingPhase::MatchingEnds {
        let mag = total.0.hypot(total.1);
        if mag < config.matching_min_force {
            let scale = config.matching_min_force / mag;
            (total.0 * scale, total.1 * scale)
        } else {
            total
        }
    } else {
        total
    };
    let raw_next = Coord {
        x: pos.x + total.0 * config.grow_time_step,
        y: pos.y + total.1 * config.grow_time_step,
    };

    // Tunneling-safe path walk (Appendix 5): a single integration step's
    // displacement is no longer bounded to a fixed, short length, so the
    // real continuous path from `pos` to `raw_next` is walked pixel by
    // pixel rather than only checking `raw_next` itself, catching the first
    // out-of-bound/high-density pixel crossed -- which becomes the Flying
    // End's actual landing spot -- and reporting every other pixel walked
    // along the way as newly claimed (`TEMPORARY_CONTOUR`) or already
    // claimed by something else.
    let outcome = raster.walk_growing_integration_step(pos, raw_next);
    let (steps, landing) = match outcome {
        StepWalkOutcome::Clear(steps) => (steps, None),
        StepWalkOutcome::Landed {
            hit: _,
            center,
            before,
        } => (before, Some(center)),
    };
    // The walk's own first reported pixel is always `pos`'s own pixel --
    // the Flying End's current position, already part of its own body
    // (either a real contour value from the original full-map fill, or
    // `TEMPORARY_CONTOUR` from this same contour's own previous step) --
    // so it always reads back as a `Conflict` against itself. Skipped
    // entirely: it needs no (re)claiming, and would otherwise trigger a
    // false "already claimed" warning every single step.
    let mut steps = steps.into_iter();
    steps.next();
    for (x, y, step) in steps {
        match step {
            PixelWalkStep::Marked => {
                temp_owner.insert((x, y), contour_idx);
            }
            PixelWalkStep::Conflict(val) => {
                // A contour revisiting its own earlier trail is expected
                // and silent (exactly today's behavior) -- whether that
                // trail is still this contour's own open `TEMPORARY_CONTOUR`
                // tail (per `temp_owner`) or, since a contour's two Flying
                // Ends resolve independently, its *other* end's already-
                // final, already-drawn real value (this same contour's own
                // `CONTOUR_0_MATRIX_VALUE + contour_idx`, written the moment
                // that end landed/closed/merged while this end was still
                // flying). Anything else -- a *different* contour's real
                // value, or another Flying End's own `TEMPORARY_CONTOUR`
                // tail -- is a genuine conflict worth warning about. Left
                // exactly as it is either way (never turned into
                // `HIGH_DENSITY`, unlike `write_contour`'s own conflict
                // rule).
                let is_own_final_value = val == CONTOUR_0_MATRIX_VALUE + contour_idx as u32;
                if !is_own_final_value && temp_owner.get(&(x, y)) != Some(&contour_idx) {
                    warnings.push(format!(
                        "Growing Process: contour {contour_idx}'s integration step at pixel \
                         ({x},{y}) found it already claimed ({val}); left it as-is"
                    ));
                }
            }
        }
    }

    match landing {
        Some(center) => {
            // This subsumes the old fixed-step algorithm's proximity snap
            // and its "landed directly on a border pixel" case into one
            // mechanism: the landing distance is no longer a fixed step
            // length, so the newly-final segment needs resampling either
            // way.
            append_node(contours, contour_idx, is_start, center);
            finalize_contour_ls(contour_idx, contours, raster, config, grown);
            dots.push(center);
            GrowStepOutcome::Resolved
        }
        None => {
            append_node(contours, contour_idx, is_start, raw_next);
            grown[contour_idx] = true;
            dots.push(raw_next);
            GrowStepOutcome::StillFlying(FlyingEnd {
                contour_idx,
                is_start,
            })
        }
    }
}

/// Whatever [`run_growing_seeking`] (Phase 1) left unresolved, carried over
/// to [`run_growing_matching`] (Phase 2).
pub struct GrowingMatchState {
    pending: std::collections::VecDeque<FlyingEnd>,
    /// Maps a `TEMPORARY_CONTOUR` pixel to the contour that placed it,
    /// spanning both phases and every Flying End -- lets `grow_one_step`
    /// tell its own earlier trail apart from a genuine conflict with a
    /// different contour.
    temp_owner: std::collections::HashMap<(i64, i64), usize>,
}

/// Step 1's Growing Process (see the doc), Phase 1 (`SeekingOutOfBound` --
/// see [`GrowingPhase`]): every Flying End gets up to
/// `growing_oob_seeking_max_steps` steps entirely on its own -- no merging,
/// no closing, `contours`/`grown` never change length here, so tracking each
/// end's own step count by its (stable) `(contour_idx, is_start)` is safe
/// for the whole pass. Flying Ends are never advanced in parallel, but
/// round-robin rather than one at a time to completion: every still-pending
/// one gets exactly one integration step, then the whole list is cycled
/// through again, and so on until every one has either resolved or spent its
/// own budget. A budget of `0` turns this phase off outright: every Flying
/// End starts straight in `MatchingEnds`.
///
/// Deliberately not run as part of `extract` itself: `--create_svg` needs to
/// write `00_..._step1.svg` (the pre-growing state, with `result`'s own
/// `pre_growing_flying_ends` as red rings) and, after
/// [`run_growing_close_search`], `01_..._step1_close_search.svg`, before this
/// runs, then `02_..._step1_growing_seeking.svg` (this function's own
/// result) after -- see the doc's Visualization section. Extends
/// `result.grown_by_growing_process` (reclaimed from wherever Close Search
/// left it, the same way [`run_growing_matching`] reclaims this function's
/// own) rather than starting it over, so a contour Close Search already
/// touched still draws blue there even if Seeking never touches it again
/// (used by that file to draw a touched contour in blue instead of green)
/// and returns any new warnings raised along the way (a
/// `walk_growing_integration_step` conflict against a different contour's
/// own pixel, see `grow_one_step`), plus the [`GrowingMatchState`]
/// [`run_growing_matching`] needs to run Phase 2.
pub fn run_growing_seeking(
    result: &mut Step1Result,
    config: &Config,
) -> (GrowingMatchState, Vec<String>) {
    let mut grown = std::mem::take(&mut result.grown_by_growing_process);
    let mut warnings = Vec::new();
    let mut push_pull_vectors = Vec::new();
    let mut dots = Vec::new();
    let mut temp_owner: std::collections::HashMap<(i64, i64), usize> =
        std::collections::HashMap::new();

    let initial_ends: std::collections::VecDeque<FlyingEnd> =
        collect_flying_ends(&result.contours, &result.raster).into();
    // Every end's own position right now, before Seeking touches any of
    // them -- `revert_seeking_growth` needs this (not a step count, which a
    // sibling end's own mid-Seeking resolve can silently invalidate, see its
    // own doc comment) for whichever ends still haven't resolved once their
    // own budget runs out.
    let originals: std::collections::HashMap<(usize, bool), Coord<f64>> = initial_ends
        .iter()
        .map(|e| {
            (
                (e.contour_idx, e.is_start),
                flying_end_position(&result.contours[e.contour_idx].lwg.ls, e.is_start),
            )
        })
        .collect();
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
            &mut dots,
            &mut temp_owner,
            &mut warnings,
        ) {
            GrowStepOutcome::Resolved => {}
            GrowStepOutcome::StillFlying(new_end) => {
                let count = steps_taken
                    .entry((new_end.contour_idx, new_end.is_start))
                    .or_insert(0);
                *count += 1;
                if *count >= config.growing_oob_seeking_max_steps {
                    let original = originals[&(new_end.contour_idx, new_end.is_start)];
                    revert_seeking_growth(
                        new_end.contour_idx,
                        new_end.is_start,
                        original,
                        &mut result.contours,
                        &mut result.raster,
                        &temp_owner,
                    );
                    matching.push_back(new_end);
                } else {
                    seeking.push_back(new_end);
                }
            }
        }
    }

    result.grown_by_growing_process = grown;
    result.growing_push_pull_vectors = push_pull_vectors;
    result.growing_integration_step_dots = dots;

    (
        GrowingMatchState {
            pending: matching,
            temp_owner,
        },
        warnings,
    )
}

/// Step 1's Growing Process (see the doc), Phase 2 (`MatchingEnds` -- see
/// [`GrowingPhase`]): the full process for whatever [`run_growing_seeking`]
/// (Phase 1) didn't resolve on its own within its own budget. Flying Ends
/// are still round-robin, never in parallel: every still-pending one gets
/// exactly one integration step, then the whole list is cycled through
/// again, and so on until none are left (resolving one, by merging two
/// contours together, can also resolve another already in the list --
/// `MatchingEnds` only). A generous, purely defensive step budget
/// (`MAX_GROWING_STEPS_PER_END` times however many Flying Ends entered this
/// phase) guards against an unbounded loop (e.g. an oscillating limit cycle
/// between two density clusters) -- not part of the doc's own algorithm,
/// which assumes every path eventually resolves; hitting it is reported as
/// a warning, with however many Flying Ends were still unresolved left
/// exactly where they were.
///
/// Extends `result.grown_by_growing_process` (reclaimed from wherever Phase
/// 1 left it) rather than starting it over, so a contour Phase 1 already
/// touched still draws blue there even if Phase 2 never touches it again;
/// `growing_push_pull_vectors`/`growing_integration_step_dots` instead start
/// fresh, empty, so that file's own push/pull-vector and
/// integration-step-dot layers show only this phase's own steps, not Phase
/// 1's already covered by its own file. Returns any new warnings raised
/// along the way, on top of whatever `run_growing_seeking` already returned
/// of its own.
pub fn run_growing_matching(
    result: &mut Step1Result,
    config: &Config,
    state: GrowingMatchState,
) -> Vec<String> {
    let GrowingMatchState {
        mut pending,
        mut temp_owner,
    } = state;
    let mut grown = std::mem::take(&mut result.grown_by_growing_process);
    let mut push_pull_vectors = Vec::new();
    let mut dots = Vec::new();
    let mut warnings = Vec::new();

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
            &mut dots,
            &mut temp_owner,
            &mut warnings,
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
    result.growing_integration_step_dots = dots;
    warnings
}

/// Runs all three of Step 1's Growing Process passes back to back, exactly
/// as [`run_growing_close_search`] followed by [`run_growing_seeking`] then
/// [`run_growing_matching`] would -- kept for callers that don't need
/// `--create_svg`'s own separate per-pass snapshots (this module's own tests
/// among them).
pub fn run_growing(result: &mut Step1Result, config: &Config) -> Vec<String> {
    run_growing_close_search(result, config);
    let (state, mut warnings) = run_growing_seeking(result, config);
    warnings.extend(run_growing_matching(result, config, state));
    warnings
}

/// Resolves every Heavy Object's own gravity reading, deferred until now so
/// each circle fit runs against a contour's *final* geometry -- must not be
/// called before [`run_growing_matching`] returns (or, equivalently,
/// [`run_growing`]).
///
/// Fitting a circle right when a Heavy Object's intersection is first found,
/// in [`extract`], could only ever see that contour's own raw, pre-growing
/// fragment: right near an open dangling end, [`point_offset`] cannot reach
/// past it, so the fit runs on a handful of points, noise-sensitive by
/// construction. If the Growing Process's own Matching phase later stitches
/// that fragment to another one end to end, the two readings -- each taken
/// independently, each starved for points near its own original dangling
/// end -- can disagree about which side is downhill even though the merged,
/// final contour's own curvature is itself perfectly consistent (see
/// `flying_end_force`'s own config comment, and `Contours-to-Raster.md`'s
/// Growing Process section, for the mechanism). Resolving it here instead,
/// against `result.raster`'s and `result.contours`' now-final state, avoids
/// that failure mode entirely.
///
/// Re-scans every one of `result.heavy_object_polygons` (already computed
/// and buffered by [`extract`]) against the raster, appending one
/// `PointGravityDefiners` reading per contour actually touched to
/// `result.point_definers` -- exactly what [`extract`]'s own `HeavyObject`
/// handling used to do immediately, just deferred this far. A stale
/// `(contour_idx, at)` pair captured back in `extract` would not do: a merge
/// can shift contour indices down and always redraws the merged contour's
/// entire raster footprint fresh (see `merge_contours`), so only a fresh
/// scan against the current raster is a correct source of truth. Returns
/// every warning found along the way (a degenerate circle fit); does not
/// touch `result.warnings` itself, the same convention
/// [`run_growing_seeking`]/[`run_growing_matching`] use, so a caller can
/// log/collect them the same way.
pub fn resolve_heavy_object_gravity(result: &mut Step1Result, config: &Config) -> Vec<String> {
    let mut warnings = Vec::new();
    for poly in &result.heavy_object_polygons {
        for (contour_idx, at) in contour_centroids_in_polygon(&result.raster, poly) {
            push_heavy_object_reading(
                &result.contours,
                contour_idx,
                at,
                config.circumference_fitting_points_number,
                &mut result.point_definers,
                &mut warnings,
            );
        }
    }
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

        let mut result = extract(&map, &config).unwrap();

        assert_eq!(
            result.heavy_object_polygons.len(),
            1,
            "one polygon per Heavy Object, drawn regardless of what it intersects"
        );
        // Heavy Object gravity is no longer resolved inside `extract` itself
        // (see `resolve_heavy_object_gravity`'s own doc comment) -- this
        // fixture has nothing to merge/close, so calling it right away,
        // without running the Growing Process first, is enough.
        let heavy_object_warnings = resolve_heavy_object_gravity(&mut result, &config);
        assert!(heavy_object_warnings.is_empty(), "{heavy_object_warnings:?}");
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
        // comfortably inside `flying_end_merge_distance` -- and they belong
        // to the very same (single) contour. The merge check should close
        // it into a ring (not corrupt it by feeding both into
        // `merge_contours` as if they were two different contours, and not
        // silently ignore the match either -- a contour's own other end is
        // a valid, and good, merge partner).
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
            growing_integration_step_dots: Vec::new(),
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
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 10.0,
            attraction_force_window: 15.0,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 2.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 6.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 5.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
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
        // start) are only 2m apart -- well inside `flying_end_merge_distance`
        // -- while each contour's own two ends stay safely outside it, so
        // this merges A with B rather than either closing on itself. Both
        // far ends already sit on the raster's own border (x=0 for A, x=39
        // for the 40-wide raster's B) and so are already out of bound before
        // growing even starts -- neither is ever a Flying End to begin with,
        // which sidesteps needing `growing_oob_seeking_max_steps` (0 here,
        // disabled outright) to grow either of them there itself: with
        // Seeking off, `MatchingEnds` alone has nothing to move a lone,
        // unmatched Flying End with (contour/out-of-bound force are
        // Seeking-only, and there is no other pending end nearby for
        // `flying_end_force` to react to), so a far end that still needed to
        // grow to its own border would never resolve.
        let ls_a = LineString::new(vec![c(0.0, 10.0), c(10.0, 10.0)]);
        let ls_b = LineString::new(vec![c(12.0, 10.0), c(39.0, 10.0)]);
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
                coord: c(39.0, 10.0),
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
            growing_integration_step_dots: Vec::new(),
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
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 10.0,
            attraction_force_window: 15.0,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 2.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 6.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 5.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
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

    /// Builds two open contours, A and B, whose dangling ends sit ~0.7m
    /// apart (close enough to merge) but whose own last few nodes, right
    /// before that dangling end, sit exactly on two *different* circles --
    /// one centered south of the gap, one centered north of it -- so a
    /// circle fit taken independently on each raw, un-merged fragment
    /// recovers opposite-pointing gravity directions, the same conflict
    /// `resolve_heavy_object_gravity`'s own doc comment describes. `mirrored`
    /// picks which side (`false` for A, `true` for B, mirrored so their far
    /// ends sit on opposite raster borders and their near ends approach each
    /// other), returning the open `LineString` in node order from its own
    /// far/border end to its own dangling end (the array's last node).
    fn heavy_object_merge_test_contour(mirrored: bool) -> LineString<f64> {
        let pt = |center: Coord<f64>, r: f64, deg: f64| {
            let a = deg.to_radians();
            c(center.x + r * a.cos(), center.y + r * a.sin())
        };
        // Angles run from `off3` (furthest from the tip) to `tip` (the
        // dangling end itself); B's own angles are the mirror image of A's,
        // reflected through the horizontal (negated), *and* reversed in
        // order -- mirroring alone would put B's tip at the wrong end of
        // its own arc (furthest from A's, not closest).
        let (center, angles, border_x) = if mirrored {
            (c(14.0, 16.0), [-65.0_f64, -75.0, -85.0, -95.0], 45.0)
        } else {
            (c(10.5, 4.0), [95.0_f64, 85.0, 75.0, 65.0], 0.0)
        };
        let r = 6.0;
        let off3 = pt(center, r, angles[0]);
        let off2 = pt(center, r, angles[1]);
        let off1 = pt(center, r, angles[2]);
        let tip = pt(center, r, angles[3]); // the dangling end itself
        let border = c(border_x, off3.y); // same y as off3, for a straight run
        LineString::new(vec![border, off3, off2, off1, tip])
    }

    #[test]
    fn heavy_object_circle_fit_on_raw_fragments_disagrees_before_a_merge() {
        let ls_a = heavy_object_merge_test_contour(false);
        let ls_b = heavy_object_merge_test_contour(true);
        let a_tip = *ls_a.0.last().unwrap();
        let b_tip = *ls_b.0.last().unwrap();
        assert!(
            (a_tip.x - b_tip.x).hypot(a_tip.y - b_tip.y) < 3.0,
            "the two dangling ends must be close enough to merge"
        );

        let contours_a = vec![Contour {
            lwg: LineWithGravity::new(ls_a),
            elevation_height: None,
        }];
        let contours_b = vec![Contour {
            lwg: LineWithGravity::new(ls_b),
            elevation_height: None,
        }];

        let mut pd_a = Vec::new();
        let mut warnings_a = Vec::new();
        push_heavy_object_reading(&contours_a, 0, a_tip, 3, &mut pd_a, &mut warnings_a);
        let mut pd_b = Vec::new();
        let mut warnings_b = Vec::new();
        push_heavy_object_reading(&contours_b, 0, b_tip, 3, &mut pd_b, &mut warnings_b);

        assert!(warnings_a.is_empty(), "{warnings_a:?}");
        assert!(warnings_b.is_empty(), "{warnings_b:?}");
        let (adx, ady) = (pd_a[0].gravity_dx.unwrap(), pd_a[0].gravity_dy.unwrap());
        let (bdx, bdy) = (pd_b[0].gravity_dx.unwrap(), pd_b[0].gravity_dy.unwrap());
        assert!(
            adx * bdx + ady * bdy < 0.0,
            "the two independently-fit readings should point in opposite directions: \
             a=({adx},{ady}), b=({bdx},{bdy})"
        );
    }

    #[test]
    fn heavy_object_gravity_resolved_after_matching_agrees_where_a_pre_merge_fit_would_have_conflicted(
    ) {
        let ls_a = heavy_object_merge_test_contour(false);
        let ls_b = heavy_object_merge_test_contour(true);

        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 46, 20);
        raster.write_contour(0, &ls_a);
        raster.write_contour(1, &ls_b);
        raster.compute_out_of_bound();

        let heavy_object_polygons = vec![
            // Straddles A's own original bend, well clear of B's.
            contour_geometry::ls_to_polygon(
                &LineString::new(vec![c(9.5, 8.5), c(9.5, 11.5)]),
                1.0,
                0.2,
            ),
            // Straddles B's own original bend, well clear of A's.
            contour_geometry::ls_to_polygon(
                &LineString::new(vec![c(16.5, 8.5), c(16.5, 11.5)]),
                1.0,
                0.2,
            ),
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
            raw_polylines: Vec::new(),
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons,
            pre_growing_flying_ends: Vec::new(),
            grown_by_growing_process: vec![false, false],
            growing_push_pull_vectors: Vec::new(),
            growing_integration_step_dots: Vec::new(),
            warnings: Vec::new(),
        };
        let config = Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 3,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 10.0,
            attraction_force_window: 15.0,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 2.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 6.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 5.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        };

        let growing_warnings = run_growing(&mut result, &config);
        assert!(growing_warnings.is_empty(), "{growing_warnings:?}");
        assert_eq!(result.contours.len(), 1, "A and B must merge into one contour");

        let heavy_object_warnings = resolve_heavy_object_gravity(&mut result, &config);
        assert!(heavy_object_warnings.is_empty(), "{heavy_object_warnings:?}");
        assert_eq!(
            result.point_definers.len(),
            2,
            "one reading per Heavy Object polygon, both against the merged contour"
        );
        for pd in &result.point_definers {
            assert_eq!(pd.reference_contour, 0);
        }

        let step2 = crate::step2_obvious_gravity::resolve(
            &mut result.contours,
            &result.point_definers,
            &result.line_definers,
        );
        assert!(
            step2.is_ok(),
            "readings taken against the final, merged contour must agree: {:?}",
            step2.err()
        );
    }

    /// A `Config` for the Close Search tests below, with only
    /// `searching_fov`/`searching_distance` varying -- everything else is a
    /// harmless placeholder, since Close Search itself never reads any of
    /// the force-curve parameters (those are Seeking/Matching-only).
    fn close_search_test_config(searching_fov: f64, searching_distance: f64) -> Config {
        Config {
            bezier_linearization_step: 0.1,
            contours_step: 2.0,
            rasterization_px_size: 1.0,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            searching_fov,
            searching_distance,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 10.0,
            attraction_force_window: 15.0,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 2.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 6.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 5.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        }
    }

    /// A minimal `Step1Result` for the Close Search tests below: everything
    /// beyond `contours`/`raster` is either unused by
    /// [`run_growing_close_search`] or a harmless empty placeholder.
    fn close_search_test_result(contours: Vec<Contour>, raster: ContourRaster) -> Step1Result {
        let grown = vec![false; contours.len()];
        let raw_polylines = vec![Vec::new(); contours.len()];
        Step1Result {
            contours,
            raw_polylines,
            raster,
            point_definers: Vec::new(),
            line_definers: Vec::new(),
            slope_lines: Vec::new(),
            slope_lines_contours_search_radius: 3.0,
            heavy_object_polygons: Vec::new(),
            pre_growing_flying_ends: Vec::new(),
            grown_by_growing_process: grown,
            growing_push_pull_vectors: Vec::new(),
            growing_integration_step_dots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn close_search_merges_two_flying_ends_of_different_contours() {
        // Mirrors `growing_merge_keeps_the_two_contours_raw_polylines_separate`'s
        // own geometry: A's end and B's start are 2m apart, well inside
        // `searching_distance`, and aimed directly at each other, while both
        // far ends already sit on the raster's own border (out of bound from
        // the start, never Flying Ends at all) -- Close Search alone should
        // already merge A and B, before Seeking or Matching ever runs.
        let ls_a = LineString::new(vec![c(0.0, 10.0), c(10.0, 10.0)]);
        let ls_b = LineString::new(vec![c(12.0, 10.0), c(39.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls_a);
        raster.write_contour(1, &ls_b);
        raster.compute_out_of_bound();

        let mut result = close_search_test_result(
            vec![
                Contour {
                    lwg: LineWithGravity::new(ls_a),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_b),
                    elevation_height: None,
                },
            ],
            raster,
        );
        let config = close_search_test_config(90.0, 5.0);

        run_growing_close_search(&mut result, &config);

        assert_eq!(result.contours.len(), 1, "A and B should have merged");
        assert!(result.grown_by_growing_process.iter().all(|&g| g));
    }

    #[test]
    fn close_search_closes_a_contours_own_two_ends_that_meet_in_its_cone() {
        // Same "V" shape as
        // `growing_closes_a_contour_whose_own_two_flying_ends_meet_each_other`:
        // both ends sit 4m apart at y=10. A wide `searching_fov` (350
        // degrees) keeps this test about the closing logic itself, not about
        // precisely aiming each tip's own forward direction at the other.
        let ls = LineString::new(vec![c(15.0, 10.0), c(17.0, 15.0), c(19.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);
        raster.compute_out_of_bound();

        let mut result = close_search_test_result(
            vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }],
            raster,
        );
        let config = close_search_test_config(350.0, 5.0);

        run_growing_close_search(&mut result, &config);

        assert_eq!(
            result.contours.len(),
            1,
            "closed, not merged away or duplicated"
        );
        assert!(
            result.contours[0].lwg.ls.is_closed(),
            "expected the contour to have been closed into a ring, got {:?}",
            result.contours[0].lwg.ls
        );
        assert_eq!(result.grown_by_growing_process, vec![true]);
    }

    #[test]
    fn close_search_rejects_a_flying_end_blocked_by_a_third_contour() {
        // Same A/B setup as the merge test above, plus a third contour C
        // running straight across the gap between them (x=11, from y=5 to
        // y=17) -- the straight connection from A's end to B's start would
        // have to cross it, so it must not be accepted as a candidate.
        let ls_a = LineString::new(vec![c(0.0, 10.0), c(10.0, 10.0)]);
        let ls_b = LineString::new(vec![c(12.0, 10.0), c(39.0, 10.0)]);
        let ls_c = LineString::new(vec![c(11.0, 5.0), c(11.0, 17.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls_a);
        raster.write_contour(1, &ls_b);
        raster.write_contour(2, &ls_c);
        raster.compute_out_of_bound();

        let mut result = close_search_test_result(
            vec![
                Contour {
                    lwg: LineWithGravity::new(ls_a),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_b),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_c),
                    elevation_height: None,
                },
            ],
            raster,
        );
        // Reaches comfortably across the 2m A-B gap, but not as far as C's
        // own tips (just over 5m from either A's end or B's start), so this
        // is purely about the blocking check, not an incidental distance/fov
        // exclusion of C's own ends.
        let config = close_search_test_config(90.0, 3.0);

        run_growing_close_search(&mut result, &config);

        assert_eq!(
            result.contours.len(),
            3,
            "A and B must not merge across C's blocking contour"
        );
    }

    #[test]
    fn close_search_lands_a_flying_end_directly_on_an_out_of_bound_pixel_ahead() {
        // A single open contour deep inside an otherwise-empty raster, far
        // from its own other end (5m, well outside `searching_distance`
        // here) -- nothing for the end-to-end search to find, so the
        // out-of-bound search takes over: virtually the whole raster besides
        // the contour's own line is out-of-bound once `compute_out_of_bound`
        // runs, so one such pixel sits just past this end's own tip.
        let ls = LineString::new(vec![c(20.0, 20.0), c(25.0, 20.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);
        raster.compute_out_of_bound();

        let mut result = close_search_test_result(
            vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }],
            raster,
        );
        let config = close_search_test_config(90.0, 3.0);

        run_growing_close_search(&mut result, &config);

        assert_eq!(
            result.contours.len(),
            1,
            "landing must not merge or duplicate the contour"
        );
        assert!(
            collect_flying_ends(&result.contours, &result.raster).is_empty(),
            "both ends should have landed directly onto an out-of-bound pixel"
        );
        assert_eq!(result.grown_by_growing_process, vec![true]);
    }

    #[test]
    fn close_search_leaves_a_flying_end_with_nothing_in_range_untouched() {
        let ls = LineString::new(vec![c(10.0, 10.0), c(20.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);
        raster.compute_out_of_bound();

        let mut result = close_search_test_result(
            vec![Contour {
                lwg: LineWithGravity::new(ls.clone()),
                elevation_height: None,
            }],
            raster,
        );
        // Smaller than the ~1m gap to the nearest out-of-bound pixel beside
        // the line, and far smaller than the 10m to this contour's own other
        // end -- nothing at all should be found.
        let config = close_search_test_config(90.0, 0.4);

        run_growing_close_search(&mut result, &config);

        assert_eq!(result.contours[0].lwg.ls.0, ls.0, "left exactly as it was");
        assert_eq!(result.grown_by_growing_process, vec![false]);
        assert_eq!(
            collect_flying_ends(&result.contours, &result.raster).len(),
            2,
            "both ends should still be flying, ready for Seeking"
        );
    }

    #[test]
    fn close_search_distance_zero_disables_the_pass() {
        let ls_a = LineString::new(vec![c(0.0, 10.0), c(10.0, 10.0)]);
        let ls_b = LineString::new(vec![c(12.0, 10.0), c(39.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls_a);
        raster.write_contour(1, &ls_b);
        raster.compute_out_of_bound();

        let mut result = close_search_test_result(
            vec![
                Contour {
                    lwg: LineWithGravity::new(ls_a),
                    elevation_height: None,
                },
                Contour {
                    lwg: LineWithGravity::new(ls_b),
                    elevation_height: None,
                },
            ],
            raster,
        );
        let config = close_search_test_config(90.0, 0.0);

        run_growing_close_search(&mut result, &config);

        assert_eq!(result.contours.len(), 2, "0.0 disables Close Search outright");
        assert_eq!(result.grown_by_growing_process, vec![false, false]);
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
            growing_integration_step_dots: Vec::new(),
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
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 5,
            contour_force_window: 6.0,
            attraction_force_window: 6.0,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 2.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 4.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 1.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
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
        // An integration step during `SeekingOutOfBound`: a Flying End
        // heading due east, with no out-of-bound/high-density pixel nearby
        // and nothing to react to but its own already-drawn, perfectly
        // collinear trailing body -- so the contour force it feels pushes
        // straight away from that body, continuing dead straight. Two
        // otherwise-identical runs, the only difference being whether some
        // *other* Flying End's own not-yet-final tail happens to sit just
        // ahead and to one side, marked `TEMPORARY_CONTOUR` by
        // `walk_growing_integration_step` exactly as Step 1's Growing
        // Process itself does for every step it takes while still flying
        // (see `grow_one_step`). Before this was wired up, a Flying End's
        // own tail was invisible to any other Flying End growing alongside
        // it until it finally resolved -- two contours seeking the border
        // independently, close and parallel, could fly right through each
        // other. With it, the second run's new node must swing measurably
        // away from that pixel instead of continuing on the same straight
        // line as the first.
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
                raster.walk_growing_integration_step(c(19.5, 51.5), c(20.0, 51.9));
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
                searching_fov: 0.0,
                searching_distance: 0.0,
                growing_oob_seeking_max_steps: 10,
                contour_force_window: 6.0,
                attraction_force_window: 6.0,
                contour_force_max_repulsion: 4.0,
                contour_force_equilibrium: 5.0,
                contour_force_max_attraction: -0.5,
                contour_force_second_equilibrium: 8.0,
                out_of_bound_force: 2.0,
                density_region_force: 2.0,
                flying_end_force: 2.0,
                flying_end_merge_distance: 1.0,
                matching_min_force: 0.0,
                grow_time_step: 1.0,
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
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
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
        // An integration step itself, on the raster it actually runs
        // against (not the hand-simulated stand-in the previous test
        // uses): once a step leaves a Flying End still flying, the segment
        // it just grew must already read back as `TEMPORARY_CONTOUR`, or a
        // second Flying End scanning its own window a moment later would
        // find nothing there to repel from at all.
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
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 10,
            contour_force_window: 6.0,
            attraction_force_window: 6.0,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 5.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 8.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 1.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
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
            &mut Vec::new(),
            &mut std::collections::HashMap::new(),
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
    fn grow_one_step_records_the_four_force_contributions_separately() {
        // Same east-heading setup as
        // `growing_step_is_deflected_by_another_flying_ends_temporary_tail`,
        // run twice, with and without one other Flying End's own temporary
        // tail nearby (repulsion, above and ahead of the straight path) --
        // isolates that one extra hit's own effect on `contour` (it must
        // not leak into any other term), and, unlike that other test,
        // checks the recorded breakdown itself rather than just the
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
                raster.walk_growing_integration_step(c(19.5, 51.5), c(20.0, 51.9));
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
                searching_fov: 0.0,
                searching_distance: 0.0,
                growing_oob_seeking_max_steps: 10,
                contour_force_window: 6.0,
                attraction_force_window: 6.0,
                contour_force_max_repulsion: 4.0,
                contour_force_equilibrium: 5.0,
                contour_force_max_attraction: -0.5,
                contour_force_second_equilibrium: 8.0,
                out_of_bound_force: 2.0,
                density_region_force: 2.0,
                flying_end_force: 2.0,
                flying_end_merge_distance: 1.0,
                matching_min_force: 0.0,
                grow_time_step: 1.0,
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
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
                &mut Vec::new(),
            );
            assert!(matches!(outcome, GrowStepOutcome::StillFlying(_)));
            assert_eq!(
                push_pull_vectors.len(),
                1,
                "exactly one integration step was taken"
            );
            push_pull_vectors[0]
        }

        let without_temp = forces_for(false);
        let with_temp = forces_for(true);

        assert_eq!(without_temp.flying_end, c(16.5, 50.5));
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
        assert_eq!(
            without_temp.flying_end_force,
            (0.0, 0.0),
            "flying_end_force never applies during SeekingOutOfBound"
        );
        assert_ne!(
            without_temp.contour,
            (0.0, 0.0),
            "the growing contour's own trailing pixels, directly behind the Flying End, \
             must already contribute a (forward-pushing) repulsion term on their own"
        );
        assert!(
            without_temp.contour.0 > 0.0 && without_temp.contour.1.abs() < 1e-9,
            "the trailing body is perfectly collinear (due west), so the repulsion it \
             contributes must point straight east with no y component: {:?}",
            without_temp.contour
        );

        // Adding the one extra temporary pixel must only move `contour` --
        // the other three terms have nothing to do with it and must come
        // out exactly the same.
        assert_eq!(with_temp.out_of_bound, without_temp.out_of_bound);
        assert_eq!(with_temp.density, without_temp.density);
        assert_eq!(with_temp.flying_end_force, without_temp.flying_end_force);
        assert!(
            with_temp.contour.1 < without_temp.contour.1 - 0.05,
            "a temporary pixel sitting above the straight path must push the y component \
             further negative than the contour's own (symmetric, y=0) trailing pixels alone \
             do: without={:?} with={:?}",
            without_temp.contour,
            with_temp.contour
        );

        assert_eq!(
            with_temp.total(),
            (
                with_temp.contour.0
                    + with_temp.out_of_bound.0
                    + with_temp.density.0
                    + with_temp.flying_end_force.0,
                with_temp.contour.1
                    + with_temp.out_of_bound.1
                    + with_temp.density.1
                    + with_temp.flying_end_force.1
            )
        );
    }

    fn window_test_config(contour_force_window: f64, attraction_force_window: f64) -> Config {
        Config {
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
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 10,
            contour_force_window,
            attraction_force_window,
            contour_force_max_repulsion: 4.0,
            contour_force_equilibrium: 2.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 4.0,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 2.0,
            flying_end_merge_distance: 1.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
        }
    }

    #[test]
    fn growing_window_hits_excludes_the_center_pixel_and_its_8_neighbors_from_contour_repulsion() {
        // Directly on `growing_window_hits`, not through `grow_one_step`:
        // three TEMPORARY_CONTOUR pixels at Chebyshev distance 0 (the
        // center itself), 1 (an immediate neighbor), and 2 from
        // `center_px` -- only the one at distance 2 should come back, even
        // with a contour window generous enough (5m) to reach all three.
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 20);
        let center = raster.pixel_center(10, 10);
        let neighbor = raster.pixel_center(11, 10);
        let farther = raster.pixel_center(12, 10);
        raster.walk_growing_integration_step(center, center);
        raster.walk_growing_integration_step(neighbor, neighbor);
        raster.walk_growing_integration_step(farther, farther);

        let config = window_test_config(5.0, 5.0);
        let hits = growing_window_hits(&raster, center, (10, 10), &config);
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
    fn contour_force_window_controls_how_far_the_repulsion_reaches() {
        // Same east-heading fixture again, this time with the one extra
        // temporary pixel placed 7m straight ahead (well past
        // contours_step's own 3m) -- a `contour_force_window` of 4m must
        // miss it entirely, while a wider one of 20m must not, even though
        // `contours_step`, `rasterization_px_size`, and
        // `attraction_force_window` are all unchanged between the two: only
        // the contour-repulsion window is under test here.
        fn contour_force_for(contour_force_window: f64, mark_far_pixel: bool) -> (f64, f64) {
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
                raster.walk_growing_integration_step(c(23.0, 50.5), c(23.5, 50.5));
            }

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let config = window_test_config(contour_force_window, 4.0);
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
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
                &mut Vec::new(),
            );
            assert!(matches!(outcome, GrowStepOutcome::StillFlying(_)));
            push_pull_vectors[0].contour
        }

        // At each window size, compare with vs without the far pixel, so
        // widening the window is the only thing that changes between the
        // two comparisons -- comparing across window sizes directly would
        // also mix in how much of the contour's own (always-visible)
        // trailing pixels each window happens to see.
        assert_eq!(
            contour_force_for(4.0, true),
            contour_force_for(4.0, false),
            "a pixel 7m away must be invisible to a contour_force_window of 4m"
        );
        assert_ne!(
            contour_force_for(20.0, true),
            contour_force_for(20.0, false),
            "the same pixel must be seen once contour_force_window is widened to 20m"
        );
    }

    #[test]
    fn flying_end_merge_distance_controls_whether_two_flying_ends_merge() {
        // Two Flying Ends (each its own contour) 7m apart -- far past
        // contours_step's own 3m -- during the Matching phase, where the
        // merge check compares their real Euclidean distance against
        // `flying_end_merge_distance`. A merge distance of 4m must not
        // match them; one of 20m must, even with `contour_force_window`
        // held fixed throughout.
        fn resolves_by_matching(flying_end_merge_distance: f64) -> bool {
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
            let mut config = window_test_config(4.0, 4.0);
            config.flying_end_merge_distance = flying_end_merge_distance;
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
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
                &mut Vec::new(),
            );
            matches!(outcome, GrowStepOutcome::Resolved)
        }

        assert!(
            !resolves_by_matching(4.0),
            "two Flying Ends 7m apart must not match through a flying_end_merge_distance of 4m"
        );
        assert!(
            resolves_by_matching(20.0),
            "the same two Flying Ends must match once flying_end_merge_distance is widened to 20m"
        );
    }

    #[test]
    fn matching_min_force_floors_a_vanishingly_small_flying_end_pull() {
        // Two separate contours' own Flying Ends, 19m apart, with
        // `attraction_force_window` at 20m: `flying_end_force`'s own cubic
        // falloff (`1 - (d / window)^3`) is deep in its tail there (t =
        // 19/20 = 0.95, falloff ~= 0.143), so the raw pull is barely an
        // eighth of `flying_end_force`'s own full magnitude -- exactly the
        // "far apart, crawling" case `matching_min_force` exists to fix. A
        // tiny `contour_force_window` keeps each end's own trailing body
        // out of it, so `flying_end_force` is the only term in play.
        fn step_displacement(matching_min_force: f64) -> (f64, f64) {
            let ls_a = LineString::new(vec![c(0.5, 50.5), c(1.5, 50.5)]);
            let ls_b = LineString::new(vec![c(40.0, 50.5), c(20.5, 50.5)]);
            let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 60, 60);
            raster.write_contour(0, &ls_a);
            raster.write_contour(1, &ls_b);

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
            let mut config = window_test_config(0.1, 20.0);
            config.flying_end_merge_distance = 1.0;
            config.matching_min_force = matching_min_force;
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
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
                &mut Vec::new(),
            );
            let next = match outcome {
                GrowStepOutcome::StillFlying(_) => *contours[0].lwg.ls.0.last().unwrap(),
                GrowStepOutcome::Resolved => panic!("expected it to still be flying"),
            };
            (next.x - 1.5, next.y - 50.5)
        }

        let (unfloored_x, unfloored_y) = step_displacement(0.0);
        assert!(unfloored_y.abs() < 1e-9, "pull is due east, no y component");
        assert!(
            (unfloored_x - 0.285_25).abs() < 1e-6,
            "raw pull at d=19, window=20, flying_end_force=2.0 must be \
             2.0 * (1 - (19/20)^3) = 0.28525m: got {unfloored_x}"
        );

        let (floored_x, floored_y) = step_displacement(1.0);
        assert!(floored_y.abs() < 1e-9, "the floor must not change direction");
        assert!(
            (floored_x - 1.0).abs() < 1e-9,
            "matching_min_force(1.0) * grow_time_step(1.0) must move exactly 1.0m \
             east, overriding the raw, much weaker pull: got {floored_x}"
        );
    }

    #[test]
    fn grow_time_step_scales_the_integration_step_distance() {
        // A single high-density pixel 5m straight ahead (a small polygon
        // stamped via `mark_high_density_polygon`, independent of the
        // border-flood machinery), well outside the distance either run's
        // own displacement will cover, and a tiny `contour_force_window`
        // (so the Flying End's own trailing body never contributes): the
        // whole net force is `density_region_force` due east, so
        // displacement is exactly `density_region_force * grow_time_step`,
        // in a straight line.
        fn step_distance(grow_time_step: f64) -> f64 {
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
            // Pixel (21, 50)'s own center, (21.5, 50.5), sits exactly 5m
            // from the Flying End at (16.5, 50.5).
            raster.mark_high_density_polygon(&Polygon::new(
                LineString::new(vec![
                    c(21.0, 50.0),
                    c(22.0, 50.0),
                    c(22.0, 51.0),
                    c(21.0, 51.0),
                    c(21.0, 50.0),
                ]),
                vec![],
            ));

            let mut contours = vec![Contour {
                lwg: LineWithGravity::new(ls),
                elevation_height: None,
            }];
            let mut point_definers = Vec::new();
            let mut grown = vec![false];
            let mut pending = std::collections::VecDeque::new();
            let mut config = window_test_config(0.5, 8.0);
            config.density_region_force = 1.0;
            config.grow_time_step = grow_time_step;
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
                // `density_region_force` only applies during `MatchingEnds`
                // (see `growing_forces`) -- `SeekingOutOfBound` drops it
                // entirely, which would leave this step with no force at
                // all.
                GrowingPhase::MatchingEnds,
                &mut push_pull_vectors,
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
                &mut Vec::new(),
            );
            let next = match outcome {
                GrowStepOutcome::StillFlying(_) => *contours[0].lwg.ls.0.last().unwrap(),
                GrowStepOutcome::Resolved => panic!("expected it to still be flying"),
            };
            (next.x - 16.5).hypot(next.y - 50.5)
        }

        assert!(
            (step_distance(1.0) - 1.0).abs() < 1e-9,
            "grow_time_step of 1.0 must move density_region_force(1.0) * 1.0 = 1.0m"
        );
        assert!(
            (step_distance(2.0) - 2.0).abs() < 1e-9,
            "grow_time_step of 2.0 must move density_region_force(1.0) * 2.0 = 2.0m"
        );
    }

    #[test]
    fn walk_growing_integration_step_lands_exactly_on_a_border_it_would_otherwise_overshoot() {
        // An out-of-bound border pixel sitting exactly 3m ahead of the
        // Flying End (the raster's own right border, width 20): a large
        // enough `out_of_bound_force` * `grow_time_step` would, left
        // unchecked, place the raw next position well past it -- the
        // tunneling-safe path walk must instead land exactly on that
        // border pixel's own center, not overshoot past it. A smaller
        // `grow_time_step` that doesn't reach the border at all must
        // instead leave it still flying, at exactly the raw displacement.
        // `attraction_force_window` is kept just past 3.0m (the target's
        // own distance) but short of 3.162m (the distance to that same
        // border column's own next-door pixels, one row up or down) --
        // since the right border is an entire out-of-bound *column*, not a
        // single pixel, a wider window would pull in several of its
        // pixels at once and break the exact math this test relies on.
        fn outcome_for(grow_time_step: f64) -> GrowStepOutcome {
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
            let mut config = window_test_config(0.5, 3.1);
            config.out_of_bound_force = 4.0;
            config.grow_time_step = grow_time_step;
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
                &mut Vec::new(),
                &mut std::collections::HashMap::new(),
                &mut Vec::new(),
            )
        }

        assert!(
            matches!(outcome_for(1.0), GrowStepOutcome::Resolved),
            "out_of_bound_force(4.0) * grow_time_step(1.0) = 4.0m overshoots the 3m border -- \
             the walk must still land exactly on it"
        );
        assert!(
            matches!(outcome_for(0.4), GrowStepOutcome::StillFlying(_)),
            "out_of_bound_force(4.0) * grow_time_step(0.4) = 1.6m falls short of the 3m border -- \
             it must still be flying"
        );
    }

    #[test]
    fn growing_raster_matches_the_final_ls_even_after_several_steps_then_closing() {
        // A downward-opening "staple": both ends start 20m apart along
        // y=10, well outside `flying_end_merge_distance`, with the rest of
        // the contour's own body (both vertical legs and the top) staying
        // well clear of the straight path directly between them -- so as
        // `flying_end_force` alone pulls the two ends straight toward each
        // other's latest position, one integration step at a time, over
        // several rounds, neither ever runs into the other's, or its own,
        // already-drawn body along the way. Closing re-samples the *whole*
        // ring against a perimeter-adjusted step (Appendix 1), which does
        // not, in general, land back on the exact intermediate points each
        // step produced -- so if those intermediate steps had each written
        // themselves into the Contour Raster as they were grown, stale
        // pixels from before that final shift would be left behind. They
        // must not be: nothing gets written until each end's own final
        // shape is known.
        let ls = LineString::new(vec![c(5.0, 10.0), c(5.0, 30.0), c(25.0, 30.0), c(25.0, 10.0)]);
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 40, 40);
        raster.write_contour(0, &ls);
        // Seal the interior first: the "staple" is open at the bottom, so
        // without this, `compute_out_of_bound`'s own flood would sneak in
        // through that gap and mark the space between the two legs (where
        // the pursuit actually happens) out of bound too.
        let mut interior = Vec::new();
        for y in 1..(raster.height as i64 - 1) {
            for x in 1..(raster.width as i64 - 1) {
                interior.push((x, y));
            }
        }
        raster.commit_flood_pixels(&interior);
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
            growing_integration_step_dots: Vec::new(),
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
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 0.1,
            attraction_force_window: 30.0,
            contour_force_max_repulsion: 2.0,
            contour_force_equilibrium: 1.0,
            contour_force_max_attraction: -0.3,
            contour_force_second_equilibrium: 2.5,
            out_of_bound_force: 2.0,
            density_region_force: 2.0,
            flying_end_force: 1.0,
            flying_end_merge_distance: 2.0,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
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
