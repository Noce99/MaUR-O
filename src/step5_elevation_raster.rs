//! Step 5: Final Tiff Computation (`Contours-to-Raster.md`). Builds the
//! Elevation 2D Vector (E2V) -- one elevation value per in-bound Contour
//! Raster (C2V) pixel -- by seeding every contour's own pixels with its own
//! `elevation_height`, running a Gravity-Guided Elevation Fill drop downhill
//! from every contour to spread a distance-weighted value into every pixel
//! along its own (possibly bent) path to its next lower neighbor (see
//! [`gravity_guided_drop_track`]), and filling whatever is left with an
//! iterative 8-connected front propagation. See [`resolve`] for the full
//! pipeline and [`write_tiff`] for turning the result into the actual file.
//!
//! Unlike every Rain Drop Production variant `step3_rain_drop` defines
//! (whose whole direction is fixed for its entire life, by design -- see
//! that module's own Rain Drop Production Definition), a Gravity-Guided
//! Elevation Fill drop's direction bends: at every step it re-reads
//! [`crate::step5_gravity_raster::GravityRaster`]'s own per-pixel downhill
//! direction (already resolved, gap-filled, and Gaussian-smoothed by the
//! time this step runs) and moves along whatever it finds there, rather
//! than whatever direction it happened to launch in. This is why it is its
//! own function here rather than one more `step3_rain_drop::Temperature`.

use std::io::BufWriter;
use std::path::Path;

use geo::Coord;
use tiff::encoder::colortype::Gray32Float;
use tiff::encoder::TiffEncoder;
use tiff::tags::Tag;

use crate::contour_raster::{ContourRaster, StepHit, CONTOUR_0_MATRIX_VALUE, OUT_OF_BOUND};
use crate::contours_to_raster_config::Config;
use crate::geotiff;
use crate::gravity_model::{contour_gravity_side, Contour};
use crate::map::Georeferencing;
use crate::step3_rain_drop::placed_sources;
use crate::step5_gravity_raster::GravityRaster;

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// A generous cap on one Gravity-Guided Elevation Fill drop's own simulated
/// steps, purely as a safety valve against an unbounded loop -- the same
/// role, and the same value, as `step3_rain_drop`'s own (private)
/// `MAX_DROP_STEPS` plays for every other rain drop. A bent path could in
/// principle wander far longer than a straight one before ever evaporating
/// (e.g. lingering near a saddle where the smoothed gravity field is weak
/// and noisy), but this run's own doc comment on [`Accum`] already explains
/// why an unusually long track can't dominate a pixel a shorter one also
/// reached -- it just ends up contributing at a vanishingly small weight
/// once it finally does evaporate.
const MAX_GRAVITY_GUIDED_STEPS: u64 = 1_000_000;

/// One E2V pixel's own accumulated evidence: a running weighted sum and
/// weight total of every value written to it, so the final value is their
/// weighted mean without keeping every individual value around (Step 5). A
/// contour's own seed value and a gap-filling round's own neighbor average
/// each count with weight `1.0`; a Gravity-Guided Elevation Fill drop's own
/// track instead weighs its value by `1.0 / track_len` (`track_len` now its
/// own whole, possibly bent, path length) -- the shorter a drop's own whole
/// path, the more its reading is trusted, specifically so a rare, very long
/// track (an open area with no closer contour along its own bent path) can
/// no longer dominate a pixel some other, much shorter, more
/// locally-relevant track also reached.
#[derive(Clone, Copy, Default)]
struct Accum {
    weighted_sum: f64,
    weight_total: f64,
}

impl Accum {
    fn add(&mut self, value: f64, weight: f64) {
        self.weighted_sum += value * weight;
        self.weight_total += weight;
    }

    fn mean(&self) -> Option<f64> {
        (self.weight_total > 0.0).then(|| self.weighted_sum / self.weight_total)
    }
}

/// Steps one Gravity-Guided Elevation Fill drop from `source`, re-reading
/// `gravity`'s own per-pixel direction at *every* step (including the
/// first) rather than committing to one direction at launch. Evaporates the
/// same way `step3_rain_drop`'s own `Fill` temperature does: an
/// out-of-bound pixel, a high-density pixel, or any contour, exempting its
/// own starting one (`source_contour_idx`) for
/// `rain_drop_starting_voting_hysteresis` steps so it can clear its own
/// source's immediate footprint. Also evaporates as a no-op -- returning
/// `None`, the same as leaving the map -- the moment it steps onto a pixel
/// `gravity` itself has no direction for (should be rare: gap-filling
/// already covers nearly every in-bound pixel) or a genuine zero vector.
///
/// Returns `None` for that no-op case, for leaving the map
/// ([`StepHit::OutOfBound`]), or for running out its own
/// [`MAX_GRAVITY_GUIDED_STEPS`] budget without evaporating meaningfully;
/// otherwise the hit and the drop's own whole path (source to evaporation,
/// every intermediate step included) -- unlike
/// `step3_rain_drop::elevation_fill_drop_track`'s own two endpoints, a bent
/// path needs every vertex, since [`resolve`] must walk it leg by leg to
/// find each pixel's own true along-path distance.
fn gravity_guided_drop_track(
    raster: &ContourRaster,
    gravity: &GravityRaster,
    source_contour_idx: u64,
    source: Coord<f64>,
    config: &Config,
) -> Option<(StepHit, Vec<Coord<f64>>)> {
    let hysteresis = config.rain_drop_starting_voting_hysteresis;
    let mut pos = source;
    let mut path = vec![pos];
    let mut steps = 0u64;

    loop {
        let (px, py) = raster.to_px(pos);
        if px < 0 || py < 0 || px as usize >= raster.width || py as usize >= raster.height {
            return None; // off the grid entirely: a no-op, same as StepHit::OutOfBound
        }
        let Some((gx, gy)) = gravity.get(px as usize, py as usize) else {
            return None; // an unreached pocket has no direction to follow: a no-op
        };
        let magnitude = gx.hypot(gy);
        if magnitude <= 0.0 {
            return None; // a zero vector has no direction to follow either
        }
        let dir = (gx / magnitude, gy / magnitude);

        let next = Coord {
            x: pos.x + dir.0 * config.rain_drop_step,
            y: pos.y + dir.1 * config.rain_drop_step,
        };
        let hit = raster.first_hit_along_step(pos, next, u64::MAX);
        let evaporate = match hit {
            None => false,
            Some(StepHit::OutOfBound) | Some(StepHit::HighDensity) => true,
            Some(StepHit::Contour(hit_idx)) => {
                !(hit_idx == source_contour_idx && steps < hysteresis)
            }
        };

        if evaporate {
            path.push(next);
            return match hit.expect("evaporate is only ever true alongside a real hit") {
                StepHit::OutOfBound => None,
                h => Some((h, path)),
            };
        }

        pos = next;
        path.push(pos);
        steps += 1;
        if steps >= MAX_GRAVITY_GUIDED_STEPS {
            return None;
        }
    }
}

/// The Elevation 2D Vector: one elevation value per Contour Raster pixel,
/// `None` for an out-of-bound pixel (never given one at all) or for a
/// genuinely unreachable in-bound pocket (see [`Step5Result::still_undefined`]).
pub struct ElevationRaster {
    grid: Vec<Vec<Option<f64>>>,
    /// Grid width, in pixels -- the same as the Contour Raster's own.
    pub width: usize,
    /// Grid height, in pixels -- the same as the Contour Raster's own.
    pub height: usize,
}

impl ElevationRaster {
    /// The elevation value at pixel `(x, y)`, or `None` if it never got
    /// one.
    pub fn get(&self, x: usize, y: usize) -> Option<f64> {
        self.grid.get(y).and_then(|row| row.get(x)).copied().flatten()
    }
}

/// What Step 5 resolved: the final [`ElevationRaster`], plus a few counts
/// worth reporting the same way every earlier step's own `Result` does.
pub struct Step5Result {
    /// The final Elevation 2D Vector.
    pub e2v: ElevationRaster,
    /// How many in-bound pixels got a value seeded directly from a
    /// contour's own `elevation_height` or written to by some
    /// Gravity-Guided Elevation Fill drop's own track, before gap-filling
    /// ran.
    pub filled_by_rain: u64,
    /// How many more in-bound pixels got a value from the gap-filling pass
    /// instead.
    pub filled_by_gap_fill: u64,
    /// How many in-bound pixels still have no value at all once
    /// gap-filling has run to completion -- a pocket the front
    /// propagation's own 8-connectivity never reached any defined pixel
    /// from.
    pub still_undefined: u64,
}

/// Runs Step 5: builds the E2V at the same size and pixel grid as `raster`,
/// seeds every contour pixel with its own contour's `elevation_height`, runs
/// one Gravity-Guided Elevation Fill drop (Rain direction only -- see the
/// doc's own note on why Anti Rain is not needed here) from every contour
/// with both a gravity direction and an elevation, and fills whatever is
/// left with iterative 8-connected front propagation. `gravity` must already
/// be fully resolved ([`crate::step5_gravity_raster::resolve`]) against this
/// same `raster`/`contours`, since every drop follows it step by step
/// instead of a direction fixed at launch. Assumes every contour still
/// without an `elevation_height` or a gravity direction has already been
/// dropped/warned about by Steps 3/4; such a contour's own raster footprint
/// is simply skipped as a source (never a sink -- a track can still land on
/// any of its pixels, but only ones that keep some other contour's own real
/// index).
pub fn resolve(
    contours: &[Contour],
    raster: &ContourRaster,
    gravity: &GravityRaster,
    config: &Config,
) -> Step5Result {
    let (width, height) = (raster.width, raster.height);
    let mut accum = vec![vec![Accum::default(); width]; height];
    let mut in_bound = vec![vec![false; width]; height];

    for y in 0..height {
        for x in 0..width {
            let val = raster.get(x as i64, y as i64);
            if val == OUT_OF_BOUND {
                continue;
            }
            in_bound[y][x] = true;
            if val >= CONTOUR_0_MATRIX_VALUE {
                let idx = (val - CONTOUR_0_MATRIX_VALUE) as usize;
                if let Some(h) = contours[idx].elevation_height {
                    accum[y][x].add(h, 1.0);
                }
            }
        }
    }

    for (c_idx, c) in contours.iter().enumerate() {
        let Some(c_height) = c.elevation_height else {
            continue;
        };
        // `side` only feeds `placed_sources`' own source-point placement
        // here -- unlike every other Rain Drop Production variant, a
        // Gravity-Guided Elevation Fill drop reads its own direction fresh
        // from `gravity` at every step (including its first), so
        // `placed_sources`' own per-source direction is simply discarded.
        let Some(side) = contour_gravity_side(c) else {
            continue;
        };
        for (source, _dir) in placed_sources(&c.lwg.ls, side, config.sources_per_contour_segment) {
            let Some((hit, path)) =
                gravity_guided_drop_track(raster, gravity, c_idx as u64, source, config)
            else {
                continue; // out of bound, an unreached gravity pocket, or ran its own step budget out
            };
            let hit_height = match hit {
                StepHit::Contour(hit_idx) => {
                    let Some(h) = contours[hit_idx as usize].elevation_height else {
                        continue; // Step 4 assumes this can't happen by Step 5; skip defensively
                    };
                    h
                }
                // No real contour to read an elevation from under a
                // high-density conflict -- suppose the drop travelled one
                // step further downhill from its own source instead (the
                // same "accordance" assumption Step 4 makes, including its
                // own Form Line half-step), per the doc's own note on why a
                // high-density hit can no longer be treated as a dead end
                // here.
                StepHit::HighDensity => c_height - c.step,
                StepHit::OutOfBound => {
                    unreachable!("gravity_guided_drop_track already turns this hit into None")
                }
            };
            let track_len: f64 = path.windows(2).map(|w| dist(w[0], w[1])).sum();
            if track_len <= 0.0 {
                continue; // a degenerate, zero-length track has nothing to interpolate along
            }
            // A track's own weight is constant along its whole length: the
            // shorter the drop's own whole (possibly bent) path, the more
            // every pixel it touches trusts its reading over a longer, less
            // locally-relevant one (see `Accum`'s own doc comment on why).
            let weight = 1.0 / track_len;
            // Walked leg by leg (each one straight, `pixels_along_segment`
            // already knows how to find every pixel a straight segment
            // touches) rather than treated as one straight segment, since
            // the path itself may now bend. `cumulative_before` is how far
            // along the whole path this leg's own start sits, so a pixel
            // found partway through it still gets its own true along-path
            // `d_a`/`d_b`, not just this one leg's own local distances --
            // this reduces to the exact original single-segment formula
            // whenever the path happens to be straight (`cumulative_before
            // == 0`, `leg_len == track_len`, one leg total).
            let mut cumulative_before = 0.0;
            for leg in path.windows(2) {
                let (leg_start, leg_end) = (leg[0], leg[1]);
                let leg_len = dist(leg_start, leg_end);
                for (px, py) in raster.pixels_along_segment(leg_start, leg_end) {
                    if px < 0 || py < 0 || px as usize >= width || py as usize >= height {
                        continue;
                    }
                    let center = raster.pixel_center(px, py);
                    let d_a = cumulative_before + dist(leg_start, center);
                    let d_b = (track_len - cumulative_before - leg_len) + dist(center, leg_end);
                    // Weight each end by the distance to the *other* end,
                    // not its own: a pixel right next to the start (d_a ~=
                    // 0) must land close to eA, so eA's own share of the
                    // blend has to grow as d_a shrinks -- i.e. it's carried
                    // by d_b, not d_a.
                    let value = (d_b * c_height + d_a * hit_height) / (d_a + d_b);
                    accum[py as usize][px as usize].add(value, weight);
                }
                cumulative_before += leg_len;
            }
        }
    }

    let filled_by_rain = count_defined(&accum, &in_bound);

    // Gap-filling: iterative 8-connected front propagation, Jacobi-style --
    // each round is computed entirely from the *previous* round's own
    // snapshot, never updated in place, so the result never depends on scan
    // order (Contours-to-Raster.md, Step 5).
    loop {
        let mut newly_filled: Vec<(usize, usize, f64)> = Vec::new();
        for y in 0..height {
            for x in 0..width {
                if !in_bound[y][x] || accum[y][x].mean().is_some() {
                    continue;
                }
                let mut sum = 0.0;
                let mut count = 0u32;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx < 0 || ny < 0 || nx as usize >= width || ny as usize >= height {
                            continue;
                        }
                        if let Some(v) = accum[ny as usize][nx as usize].mean() {
                            sum += v;
                            count += 1;
                        }
                    }
                }
                if count > 0 {
                    newly_filled.push((x, y, sum / count as f64));
                }
            }
        }
        if newly_filled.is_empty() {
            break;
        }
        for (x, y, v) in newly_filled {
            accum[y][x].add(v, 1.0);
        }
    }

    let filled_after_gap_fill = count_defined(&accum, &in_bound);
    let total_in_bound: u64 = in_bound
        .iter()
        .flatten()
        .filter(|&&b| b)
        .count() as u64;

    let grid: Vec<Vec<Option<f64>>> = accum
        .into_iter()
        .map(|row| row.into_iter().map(|a| a.mean()).collect())
        .collect();
    let grid = gaussian_smooth(&grid, config.elevation_gaussian_kernel_size);

    Step5Result {
        e2v: ElevationRaster { grid, width, height },
        filled_by_rain,
        filled_by_gap_fill: filled_after_gap_fill - filled_by_rain,
        still_undefined: total_in_bound - filled_after_gap_fill,
    }
}

/// Gaussian-smooths `grid`'s own elevation field: every pixel that already
/// has a value gets, as its final value, a Gaussian-weighted average of
/// itself and every same-defined neighbor within `kernel_size`'s own window
/// (`config.elevation_gaussian_kernel_size`) -- radius `(kernel_size - 1) /
/// 2`, standard deviation `radius / 3.0` so the window's own edge sits at
/// roughly three standard deviations, normalized by however much of that
/// weight actually landed on a defined neighbor (a pixel near an
/// out-of-bound edge or a still-undefined pocket is averaged over fewer
/// neighbors, not diluted by them reading as zero). A pixel with no value of
/// its own is left `None` -- this smooths noise among already-resolved
/// pixels; it does not fill new ones (gap-filling above already did that).
/// `kernel_size <= 1` is a no-op, returning `grid` unchanged, since a
/// zero-radius window has nothing else to average with anyway. The exact
/// same algorithm as `step5_gravity_raster`'s own (private) `gaussian_smooth`,
/// just over a scalar instead of a 2D vector -- not shared as one generic
/// function since the two live in different modules over different `Accum`
/// types, and duplicating this small a loop is cheaper than the abstraction
/// it would take to unify them.
fn gaussian_smooth(grid: &[Vec<Option<f64>>], kernel_size: usize) -> Vec<Vec<Option<f64>>> {
    let height = grid.len();
    let width = grid.first().map_or(0, Vec::len);
    if kernel_size <= 1 {
        return grid.to_vec();
    }
    let radius = (kernel_size / 2) as i64;
    let sigma = radius as f64 / 3.0;

    let mut smoothed = vec![vec![None; width]; height];
    for y in 0..height {
        for x in 0..width {
            if grid[y][x].is_none() {
                continue;
            }
            let (mut sum, mut weight_total) = (0.0, 0.0);
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx as usize >= width || ny as usize >= height {
                        continue;
                    }
                    let Some(v) = grid[ny as usize][nx as usize] else {
                        continue;
                    };
                    let squared_dist = (dx * dx + dy * dy) as f64;
                    let weight = (-squared_dist / (2.0 * sigma * sigma)).exp();
                    sum += v * weight;
                    weight_total += weight;
                }
            }
            // `weight_total` is always > 0 here: (dx, dy) = (0, 0) is always
            // in range and always weight 1.0, and `grid[y][x]` (that same
            // pixel) is already known `Some` from the check above.
            smoothed[y][x] = Some(sum / weight_total);
        }
    }
    smoothed
}

fn count_defined(accum: &[Vec<Accum>], in_bound: &[Vec<bool>]) -> u64 {
    let mut n = 0u64;
    for (row, in_bound_row) in accum.iter().zip(in_bound) {
        for (a, &b) in row.iter().zip(in_bound_row) {
            if b && a.mean().is_some() {
                n += 1;
            }
        }
    }
    n
}

/// GeoTIFF's own `ModelPixelScaleTag`/`ModelTiepointTag`/`GeoKeyDirectoryTag`/
/// `GDAL_NODATA` tag numbers -- none of them baseline TIFF tags the `tiff`
/// crate already knows by name, so every one goes through `Tag::Unknown`
/// (see `crate::geotiff`'s own doc comment for why nothing else builds one
/// for us).
const MODEL_PIXEL_SCALE_TAG: u16 = 33550;
const MODEL_TIEPOINT_TAG: u16 = 33922;
const GEO_KEY_DIRECTORY_TAG: u16 = 34735;
const GDAL_NODATA_TAG: u16 = 42113;

/// Bilinear-samples `e2v` at a continuous local ground-meter coordinate,
/// `origin`/`px_size` the same `ContourRaster` convention `e2v` was resolved
/// against (`origin` is pixel `(0, 0)`'s own corner, not its center --
/// `pixel_center`'s own `+ 0.5`). `None` outside the raster's own footprint
/// or wherever all four surrounding pixels are themselves `None`; falls back
/// to the nearer single pixel when only some of the four are.
fn sample_e2v_bilinear(e2v: &ElevationRaster, origin: Coord<f64>, px_size: f64, local: Coord<f64>) -> Option<f64> {
    let fx = (local.x - origin.x) / px_size - 0.5;
    let fy = (local.y - origin.y) / px_size - 0.5;
    if fx < -0.5 || fy < -0.5 || fx > e2v.width as f64 - 0.5 || fy > e2v.height as f64 - 0.5 {
        return None;
    }
    let c0 = fx.floor().clamp(0.0, (e2v.width - 1) as f64) as usize;
    let r0 = fy.floor().clamp(0.0, (e2v.height - 1) as f64) as usize;
    let c1 = (c0 + 1).min(e2v.width - 1);
    let r1 = (r0 + 1).min(e2v.height - 1);
    let tx = (fx - c0 as f64).clamp(0.0, 1.0);
    let ty = (fy - r0 as f64).clamp(0.0, 1.0);

    match (e2v.get(c0, r0), e2v.get(c1, r0), e2v.get(c0, r1), e2v.get(c1, r1)) {
        (Some(v00), Some(v01), Some(v10), Some(v11)) => {
            let top = v00 + (v01 - v00) * tx;
            let bottom = v10 + (v11 - v10) * tx;
            Some(top + (bottom - top) * ty)
        }
        _ => e2v.get(if tx < 0.5 { c0 } else { c1 }, if ty < 0.5 { r0 } else { r1 }),
    }
}

/// Writes `e2v` as a single-band 32-bit float TIFF -- each pixel's own
/// `elevation_height` (an integer band count relative to an arbitrary zero,
/// `Contours-to-Raster.md`'s own opening line) times `equidistance`, turning
/// that count into an actual elevation in meters, still up to that same
/// unknown constant baseline; `f32::NAN` for a pixel that never got one
/// (always an out-of-bound pixel, only ever a genuinely unreachable in-bound
/// pocket otherwise), also declared as this file's own `GDAL_NODATA` value so
/// a GIS tool doesn't have to guess.
///
/// `origin`/`px_size` are the same `ContourRaster` fields `e2v` was resolved
/// against (`raster.origin`, `raster.px_size`); `georeferencing` is the
/// source map's own [`Georeferencing`], if it has one. Real GeoTIFF tags
/// (see `crate::geotiff`) are only ever written when `georeferencing` is
/// `Some`, names a real projected CRS (`epsg != 0`), and that EPSG code fits
/// a plain SHORT GeoKey value (`u16`, vanishingly unlikely to matter for any
/// real projected CRS a map would actually use) -- otherwise this falls back
/// to exactly the plain, tagless, un-resampled TIFF this function always
/// used to write, since there is no real-world place left to put it.
///
/// When georeferenced, `e2v` (which lives in the map's own rotated
/// ground-meter frame -- grivated whenever the map itself is) is resampled
/// onto a plain axis-aligned grid in the projected CRS
/// ([`geotiff::resample_north_up`]) *before* writing, so the file on disk
/// carries an ordinary `ModelPixelScaleTag`/`ModelTiepointTag`, never a
/// rotated `ModelTransformationTag` -- see `crate::geotiff`'s own doc
/// comment for why a rotated file used to come back silently misrotated
/// wherever it was re-opened.
pub fn write_tiff(
    e2v: &ElevationRaster,
    origin: Coord<f64>,
    px_size: f64,
    equidistance: f64,
    georeferencing: Option<&Georeferencing>,
    path: &Path,
) -> Result<(), String> {
    let epsg_u16 = georeferencing.and_then(|g| u16::try_from(g.epsg).ok());
    let north_up = match (georeferencing, epsg_u16) {
        (Some(georef), Some(_)) => geotiff::resample_north_up(
            e2v.width,
            e2v.height,
            origin,
            px_size,
            |local| {
                sample_e2v_bilinear(e2v, origin, px_size, local)
                    .map(|v| (v * equidistance) as f32)
                    .unwrap_or(f32::NAN)
            },
            georef,
        ),
        _ => None,
    };

    let (width, height, data) = match &north_up {
        Some(raster) => (raster.width, raster.height, raster.data.clone()),
        None => {
            let mut data = Vec::with_capacity(e2v.width * e2v.height);
            for y in 0..e2v.height {
                for x in 0..e2v.width {
                    data.push(e2v.get(x, y).map(|v| (v * equidistance) as f32).unwrap_or(f32::NAN));
                }
            }
            (e2v.width, e2v.height, data)
        }
    };

    let file =
        std::fs::File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut encoder = TiffEncoder::new(BufWriter::new(file))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut image = encoder
        .new_image::<Gray32Float>(width as u32, height as u32)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    image
        .encoder()
        .write_tag(Tag::Unknown(GDAL_NODATA_TAG), "nan")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    if let (Some(raster), Some(epsg)) = (&north_up, epsg_u16) {
        image
            .encoder()
            .write_tag(
                Tag::Unknown(MODEL_PIXEL_SCALE_TAG),
                &[raster.px_size, raster.px_size, 0.0][..],
            )
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        image
            .encoder()
            .write_tag(
                Tag::Unknown(MODEL_TIEPOINT_TAG),
                &[0.0, 0.0, 0.0, raster.origin.x, raster.origin.y, 0.0][..],
            )
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        image
            .encoder()
            .write_tag(
                Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
                &geotiff::geo_key_directory(epsg)[..],
            )
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }

    image
        .write_data(&data)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A hypsometric ("terrain") colormap stop: an elevation ratio in `[0, 1]`
/// (this run's own lowest E2V value mapped to `0.0`, its own highest to
/// `1.0`) and the RGB color at that ratio. [`terrain_color`] linearly
/// interpolates between consecutive stops. The classic DEM look: blue for
/// low ground, through green and yellow, browns for high ground, white at
/// the very top (a snow-capped peak) -- chosen over a plain two-color
/// gradient specifically so this file reads as an actual terrain picture at
/// a glance, unlike `07_<map_name>_step4.svg`'s own blue-to-red debug
/// gradient (a different file, for a different purpose: showing each
/// contour's own relative height in a schematic diagram, not what the
/// ground actually looks like).
const TERRAIN_STOPS: [(f64, [u8; 3]); 6] = [
    (0.0, [0x32, 0x88, 0xbd]), // blue
    (0.2, [0x66, 0xc2, 0xa5]), // green
    (0.4, [0xff, 0xff, 0xbf]), // yellow
    (0.6, [0xd9, 0x78, 0x4a]), // brown
    (0.8, [0x8c, 0x52, 0x32]), // dark brown
    (1.0, [0xff, 0xff, 0xff]), // white
];

/// [`TERRAIN_STOPS`] linearly interpolated at `t` (clamped to `[0, 1]`).
fn terrain_color(t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    for pair in TERRAIN_STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return std::array::from_fn(|i| lerp_u8(c0[i], c1[i], f));
        }
    }
    TERRAIN_STOPS[TERRAIN_STOPS.len() - 1].1
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

/// Writes `e2v` as a human-readable, hypsometric-colored PNG -- unlike
/// [`write_tiff`]'s own actual data, this is a `--create_svg` diagnostic
/// only, meant to be looked at directly rather than loaded back into a GIS
/// tool. Every pixel with a value is colored by [`terrain_color`], scaled so
/// this run's own lowest E2V value maps to the bottom of the colormap and
/// its own highest to the top (a run with only one distinct value colors
/// every pixel at the colormap's own low end, rather than dividing by
/// zero). A pixel with no value at all -- always out-of-bound, only ever a
/// genuinely unreachable in-bound pocket otherwise -- is drawn black, the
/// same color the per-step SVGs already use for out-of-bound area.
pub fn write_colored_png(e2v: &ElevationRaster, path: &Path) -> Result<(), String> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for y in 0..e2v.height {
        for x in 0..e2v.width {
            if let Some(v) = e2v.get(x, y) {
                min = min.min(v);
                max = max.max(v);
            }
        }
    }

    let mut pixels = Vec::with_capacity(e2v.width * e2v.height * 3);
    for y in 0..e2v.height {
        for x in 0..e2v.width {
            let rgb = match e2v.get(x, y) {
                Some(v) if max > min => terrain_color((v - min) / (max - min)),
                Some(_) => TERRAIN_STOPS[0].1,
                None => [0, 0, 0],
            };
            pixels.extend_from_slice(&rgb);
        }
    }

    let file =
        std::fs::File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        e2v.width as u32,
        e2v.height as u32,
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    writer
        .write_image_data(&pixels)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    writer
        .finish()
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity_model::LineWithGravity;
    use geo::{LineString, Polygon};

    #[test]
    fn accum_mean_is_none_until_something_is_added() {
        assert!(Accum::default().mean().is_none());
    }

    #[test]
    fn accum_weighted_mean_favors_the_higher_weight_contribution() {
        let mut a = Accum::default();
        a.add(0.0, 1.0); // e.g. a long track (small 1.0 / track_len weight)
        a.add(10.0, 9.0); // e.g. a short track (large 1.0 / track_len weight)
        // A plain average would land on 5.0; the weighted mean must land
        // much closer to the higher-weight (shorter-track) reading instead.
        let mean = a.mean().unwrap();
        assert!(
            (mean - 9.0).abs() < 1e-9,
            "expected the weighted mean to favor the weight-9 reading, got {mean}"
        );
    }

    #[test]
    fn gaussian_smooth_is_a_no_op_at_kernel_size_one() {
        let grid = vec![vec![Some(3.0), None, Some(1.0)], vec![None, Some(-2.0), None]];
        assert_eq!(gaussian_smooth(&grid, 1), grid);
        assert_eq!(gaussian_smooth(&grid, 0), grid);
    }

    #[test]
    fn gaussian_smooth_never_gives_a_value_to_an_undefined_pixel() {
        let grid = vec![vec![Some(3.0), Some(3.0), None]];
        let smoothed = gaussian_smooth(&grid, 5);
        assert_eq!(smoothed[0][2], None, "no value of its own to smooth from");
    }

    #[test]
    fn gaussian_smooth_is_unaffected_by_a_wholly_undefined_neighborhood() {
        let mut grid = vec![vec![None; 5]; 5];
        grid[2][2] = Some(7.5);
        let smoothed = gaussian_smooth(&grid, 5);
        assert_eq!(smoothed[2][2], Some(7.5));
    }

    #[test]
    fn gaussian_smooth_blends_toward_a_defined_neighbors_own_value() {
        let grid = vec![vec![Some(0.0), Some(10.0)]];
        let smoothed = gaussian_smooth(&grid, 5);
        let s0 = smoothed[0][0].unwrap();
        let s1 = smoothed[0][1].unwrap();
        assert!(
            s0 > 0.0 && s0 < 10.0,
            "expected pixel 0 to lean toward its neighbor's 10.0 without reaching it, got {s0}"
        );
        assert!(
            s1 < 10.0 && s1 > 0.0,
            "expected pixel 1 to lean toward its neighbor's 0.0 without reaching it, got {s1}"
        );
    }

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn default_config() -> Config {
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
            growing_visualization_push_pull_vectors_scale: 1.0,
            gravity_gaussian_kernel_size: 5,
            elevation_gaussian_kernel_size: 5,
        }
    }

    fn straight_ls(y: f64) -> LineString<f64> {
        LineString::new(vec![c(0.0, y), c(20.0, y)])
    }

    fn contour_with_gravity(y: f64, gravity_dy: f64, height: f64) -> Contour {
        let mut contour = Contour {
            lwg: LineWithGravity::new(straight_ls(y)),
            step: 1.0,
            elevation_height: Some(height),
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(gravity_dy);
        contour
    }

    // Deliberately does *not* call `compute_out_of_bound`: these two open,
    // finite-length parallel lines don't actually enclose anything (their
    // own ends leave the region around and between them connected straight
    // out to the raster's own border), so running it here would flood
    // almost the whole grid to out-of-bound -- exactly the failure mode
    // `Contours-to-Raster.md`'s Step 1 flood-fills every contour's own
    // `NO_CONTOUR_IN_BOUND` area *before* computing out-of-bound to guard
    // against. These tests only check elevation values, not
    // in-bound/out-of-bound semantics, so they don't need it (see
    // `out_of_bound_pixels_never_get_a_value` below for a raster that does).
    fn raster_for(contours: &[Contour]) -> ContourRaster {
        let mut r = ContourRaster::new(c(-5.0, -20.0), 0.5, 60, 100);
        for (i, contour) in contours.iter().enumerate() {
            r.write_contour(i as u64, &contour.lwg.ls);
        }
        r
    }

    /// Every test below needs a resolved [`GravityRaster`] to hand
    /// [`resolve`] before it can build its own drop paths -- computed
    /// against the exact same `contours`/`raster`/`config` the elevation
    /// call itself then uses, the same way `src/bin/contours_to_raster.rs`
    /// chains the two.
    fn gravity_for(contours: &[Contour], raster: &mut ContourRaster, config: &Config) -> GravityRaster {
        crate::step5_gravity_raster::resolve(contours, raster, config)
    }

    #[test]
    fn gravity_guided_drop_track_bends_with_the_gravity_field() {
        // A synthetic, hand-built gravity field (bypassing
        // `step5_gravity_raster::resolve` entirely, via `GravityRaster::for_test`
        // -- see that constructor's own doc comment): due east for x < 10,
        // due south for x >= 10, an deliberate "L" turn no straight-line
        // drop could ever trace. A target contour sits south of the turn, so
        // only a drop that actually turns can ever reach it.
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 20);
        let target = LineString::new(vec![c(10.0, 15.5), c(15.0, 15.5)]);
        raster.write_contour(0, &target);

        let grid: Vec<Vec<Option<(f64, f64)>>> = (0..20)
            .map(|_y| {
                (0..20)
                    .map(|x| Some(if x < 10 { (1.0, 0.0) } else { (0.0, 1.0) }))
                    .collect()
            })
            .collect();
        let gravity = GravityRaster::for_test(grid, 20, 20);

        let config = default_config();
        let (hit, path) = gravity_guided_drop_track(
            &raster,
            &gravity,
            99, // no contour at this index: nothing to self-exempt against
            c(2.5, 2.5),
            &config,
        )
        .expect("a drop following an eastward-then-southward field must reach the target");

        assert_eq!(hit, StepHit::Contour(0));
        assert!(
            path.iter().any(|p| p.x > 9.5),
            "expected the path to actually cross into the eastern region: {path:?}"
        );
        assert!(
            path.last().unwrap().y > 10.0,
            "expected the path to travel south once inside the eastern region: {path:?}"
        );
        // Non-collinearity check: the source, the point where it enters the
        // eastern (southward) region, and the final landing point must not
        // all lie on one straight line -- a plain cross product of the two
        // legs is zero only if they do.
        let turn = path
            .iter()
            .find(|p| p.x > 9.5)
            .copied()
            .expect("already asserted this point exists above");
        let end = *path.last().unwrap();
        let (v1x, v1y) = (turn.x - path[0].x, turn.y - path[0].y);
        let (v2x, v2y) = (end.x - turn.x, end.y - turn.y);
        let cross = v1x * v2y - v1y * v2x;
        assert!(cross.abs() > 1.0, "expected a genuine bend, not a straight line: cross={cross}");
    }

    #[test]
    fn gravity_guided_drop_track_evaporates_as_a_no_op_when_gravity_is_undefined_at_the_source() {
        let raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 20);
        let grid = vec![vec![None; 20]; 20]; // no direction anywhere
        let gravity = GravityRaster::for_test(grid, 20, 20);

        let result =
            gravity_guided_drop_track(&raster, &gravity, 0, c(2.5, 2.5), &default_config());
        assert!(result.is_none());
    }

    #[test]
    fn contour_pixels_are_seeded_with_their_own_elevation() {
        let contours = vec![contour_with_gravity(10.0, -1.0, 3.0)];
        let mut raster = raster_for(&contours);
        let config = default_config();
        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        let (px, py) = raster.to_px(c(5.0, 10.0));
        assert_eq!(result.e2v.get(px as usize, py as usize), Some(3.0));
    }

    #[test]
    fn a_drop_evaporating_on_high_density_assumes_one_step_below_its_own_source() {
        // A single contour (height 5) with nothing else nearby except a
        // high-density band directly downhill of it -- every drop it fires
        // must evaporate there (no real contour to reach instead), assuming
        // a fallback elevation of 5 - 1 = 4 for it. A single source
        // (`sources_per_contour_segment = 1`, placed at the segment's own
        // start node, x = 0) keeps this deterministic: exactly one straight,
        // purely-vertical track to reason about, rather than several
        // differently-angled ones blending together at any given pixel.
        let contours = vec![contour_with_gravity(10.0, -1.0, 5.0)];
        let mut raster = raster_for(&contours);
        let band = Polygon::new(
            LineString::new(vec![
                c(0.0, 4.0),
                c(20.0, 4.0),
                c(20.0, 6.0),
                c(0.0, 6.0),
                c(0.0, 4.0),
            ]),
            vec![],
        );
        raster.mark_high_density_polygon(&band);
        let mut config = default_config();
        config.sources_per_contour_segment = 1;

        // Learn exactly where this one deterministic drop actually lands --
        // Appendix 4's own stepped walk reports the step's raw endpoint as
        // the landing spot, not the precise crossing point, so this is more
        // robust than assuming a hand-picked coordinate is close enough.
        let (hit, _, end) =
            crate::step3_rain_drop::elevation_fill_drop_track(&mut raster, 0, c(0.0, 10.0), (0.0, -1.0), &config)
                .unwrap();
        assert_eq!(hit, StepHit::HighDensity);

        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        // Right at the drop's own landing pixel: close to the fallback
        // value, 4.
        let (px, py) = raster.to_px(end);
        let near_band = result.e2v.get(px as usize, py as usize).unwrap();
        assert!(
            (near_band - 4.0).abs() < 0.5,
            "expected a value close to the fallback 4.0 right at the drop's own landing pixel, \
             got {near_band}"
        );

        // Halfway between the contour (y = 10, height 5) and the drop's own
        // landing point (fallback height 4), same x: close to their mean,
        // 4.5.
        let (px, py) = raster.to_px(c(0.0, (10.0 + end.y) / 2.0));
        let midway = result.e2v.get(px as usize, py as usize).unwrap();
        assert!(
            (midway - 4.5).abs() < 0.5,
            "expected a value close to 4.5 midway between the contour and the landing point, \
             got {midway}"
        );
    }

    #[test]
    fn a_pixel_midway_between_two_contours_gets_the_average_of_their_two_heights() {
        // Two parallel contours 10m apart, downhill = -y: a pixel exactly
        // halfway between them should land close to the mean of the two
        // heights, since dA and dB there are equal.
        let contours = vec![
            contour_with_gravity(10.0, -1.0, 0.0),
            contour_with_gravity(0.0, -1.0, -10.0),
        ];
        let mut raster = raster_for(&contours);
        let config = default_config();
        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        let (px, py) = raster.to_px(c(5.0, 5.0));
        let value = result.e2v.get(px as usize, py as usize).unwrap();
        assert!(
            (value - (-5.0)).abs() < 1.0,
            "expected a value near -5.0 (the mean of 0.0 and -10.0), got {value}"
        );
    }

    #[test]
    fn a_pixel_near_the_start_lands_near_the_starting_contours_own_height_not_the_far_one() {
        // Same two parallel contours as above, but this time querying right
        // next to the top one (height 0) instead of the midpoint -- a case
        // the midpoint test above can't distinguish a correct interpolation
        // from one that swapped which distance weighs which elevation,
        // since dA and dB are equal there. `sources_per_contour_segment = 1`
        // keeps the drop's own track deterministic (see the high-density
        // test above for why).
        let contours = vec![
            contour_with_gravity(10.0, -1.0, 0.0),
            contour_with_gravity(0.0, -1.0, -10.0),
        ];
        let mut raster = raster_for(&contours);
        let mut config = default_config();
        config.sources_per_contour_segment = 1;
        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        // Just below the top contour (height 0): must land close to 0, not
        // close to -10 (the far contour a swapped formula would produce).
        let (px, py) = raster.to_px(c(0.0, 9.5));
        let value = result.e2v.get(px as usize, py as usize).unwrap();
        assert!(
            value > -2.0,
            "expected a value close to 0.0 (the nearby contour's own height) right below it, \
             got {value}, suspiciously close to the far contour's -10.0 instead"
        );
    }

    #[test]
    fn out_of_bound_pixels_never_get_a_value() {
        // A closed ring genuinely encloses its own interior, so
        // `compute_out_of_bound`'s own border flood correctly stops at it
        // rather than leaking through (unlike `raster_for`'s open parallel
        // lines above).
        let ring = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 10.0),
            c(0.0, 10.0),
            c(0.0, 0.0),
        ]);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ring),
            step: 1.0,
            elevation_height: Some(5.0),
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(-1.0);
        let contours = vec![contour];
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 40, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        raster.compute_out_of_bound();

        let config = default_config();
        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        assert_eq!(result.e2v.get(0, 0), None, "the raster's own border is out of bound");
    }

    #[test]
    fn gap_filling_reaches_a_pixel_no_rain_drop_track_could() {
        // A single closed ring (a hilltop): every Elevation Fill drop from
        // it travels *away* from the ring's own interior (downhill), so the
        // interior itself is only ever reached by gap-filling.
        let ring = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 10.0),
            c(0.0, 10.0),
            c(0.0, 0.0),
        ]);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ring),
            step: 1.0,
            elevation_height: Some(5.0),
            empty_progeny: false,
        };
        // The ring is wound counter-clockwise, so "right of the direction
        // of travel" is consistently outward at every edge; straight down
        // (0.0, -1.0), read against the first segment's own tangent
        // ((0,0) -> (10,0), i.e. +x), is exactly that side -- downhill
        // points outward all the way around, per the doc's own hill
        // convention (Step 2).
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(-1.0);
        let contours = vec![contour];
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 40, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        raster.compute_out_of_bound();

        let config = default_config();
        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        let (px, py) = raster.to_px(c(5.0, 5.0)); // dead center of the ring
        let center = result.e2v.get(px as usize, py as usize).unwrap();
        // Close to, rather than bit-exact, the ring's own height: Gaussian
        // smoothing's own weighted average introduces harmless
        // floating-point noise even over an otherwise perfectly uniform
        // field.
        assert!(
            (center - 5.0).abs() < 1e-6,
            "the ring's own interior must be reached by gap-filling, at close to the ring's own \
             height, got {center}"
        );
        assert!(result.filled_by_gap_fill > 0);
    }

    #[test]
    fn terrain_color_hits_every_named_stop_exactly() {
        for &(t, color) in &TERRAIN_STOPS {
            assert_eq!(terrain_color(t), color);
        }
    }

    #[test]
    fn terrain_color_interpolates_between_two_consecutive_stops() {
        // Halfway between the blue stop (0.0) and the green one (0.2).
        let mid = terrain_color(0.1);
        let (blue, green) = (TERRAIN_STOPS[0].1, TERRAIN_STOPS[1].1);
        for i in 0..3 {
            let expected = (blue[i] as i32 + green[i] as i32) / 2;
            assert!(
                (mid[i] as i32 - expected).abs() <= 1,
                "channel {i}: expected close to {expected}, got {}",
                mid[i]
            );
        }
    }

    #[test]
    fn terrain_color_clamps_outside_zero_one() {
        assert_eq!(terrain_color(-5.0), TERRAIN_STOPS[0].1);
        assert_eq!(terrain_color(5.0), TERRAIN_STOPS[TERRAIN_STOPS.len() - 1].1);
    }

    #[test]
    fn write_colored_png_produces_a_valid_png_of_the_right_size_with_black_out_of_bound_pixels() {
        // A closed ring, exactly like `out_of_bound_pixels_never_get_a_value`
        // above, so the raster's own border is genuinely out-of-bound rather
        // than merely never-touched.
        let ring = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 10.0),
            c(0.0, 10.0),
            c(0.0, 0.0),
        ]);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ring),
            step: 1.0,
            elevation_height: Some(5.0),
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(-1.0);
        let contours = vec![contour];
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 40, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        raster.compute_out_of_bound();

        let config = default_config();
        let gravity = gravity_for(&contours, &mut raster, &config);
        let result = resolve(&contours, &raster, &gravity, &config);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevation.png");
        write_colored_png(&result.e2v, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let decoder = png::Decoder::new(file);
        let mut reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.width as usize, result.e2v.width);
        assert_eq!(info.height as usize, result.e2v.height);
        assert_eq!(info.color_type, png::ColorType::Rgb);

        let mut buf = vec![0u8; reader.output_buffer_size()];
        reader.next_frame(&mut buf).unwrap();
        // (0, 0) is on the raster's own border, which `compute_out_of_bound`
        // must have reached (nothing else claims it), so it must come out
        // black.
        assert_eq!(&buf[0..3], &[0, 0, 0]);
    }

    fn tiny_e2v() -> ElevationRaster {
        ElevationRaster {
            grid: vec![vec![Some(1.0), Some(2.0)], vec![Some(3.0), None]],
            width: 2,
            height: 2,
        }
    }

    #[test]
    fn write_tiff_without_georeferencing_writes_no_geo_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevation.tif");
        write_tiff(&tiny_e2v(), c(0.0, 0.0), 1.0, 1.0, None, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(file).unwrap();
        assert!(decoder
            .find_tag(Tag::Unknown(GEO_KEY_DIRECTORY_TAG))
            .unwrap()
            .is_none());
        assert!(decoder
            .find_tag(Tag::Unknown(MODEL_PIXEL_SCALE_TAG))
            .unwrap()
            .is_none());
        assert!(decoder
            .find_tag(Tag::Unknown(MODEL_TIEPOINT_TAG))
            .unwrap()
            .is_none());
        assert_eq!(
            decoder.get_tag_ascii_string(Tag::Unknown(GDAL_NODATA_TAG)).unwrap(),
            "nan"
        );
    }

    #[test]
    fn write_tiff_without_a_real_crs_falls_back_to_no_geo_tags() {
        // `epsg == 0` -- a `<projected_crs>` naming no real CRS
        // (`xml_writer.rs`'s own "Local" default) -- must fall back exactly
        // like `georeferencing: None` above, not write a bogus GeoKey
        // directory with a meaningless CRS code of 0.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevation.tif");
        let georef = Georeferencing::default(); // epsg == 0
        write_tiff(&tiny_e2v(), c(0.0, 0.0), 1.0, 1.0, Some(&georef), &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(file).unwrap();
        assert!(decoder
            .find_tag(Tag::Unknown(GEO_KEY_DIRECTORY_TAG))
            .unwrap()
            .is_none());
    }

    #[test]
    fn write_tiff_with_georeferencing_writes_the_expected_geo_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevation.tif");
        let georef = Georeferencing {
            scale: 15000,
            epsg: 3006,
            ref_point_x: 322500.0,
            ref_point_y: 6397500.0,
            grivation: 7.1,
            grivation_specified: true,
            auxiliary_scale_factor: 1.000014,
        };
        let origin = c(2252.39, -2312.17);
        let px_size = 1.0;
        let e2v = tiny_e2v();
        write_tiff(&e2v, origin, px_size, 1.0, Some(&georef), &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(file).unwrap();
        let keys = decoder.get_tag_u16_vec(Tag::Unknown(GEO_KEY_DIRECTORY_TAG)).unwrap();
        assert_eq!(keys, geotiff::geo_key_directory(3006));

        // A plain axis-aligned ModelPixelScale/ModelTiepoint pair, not a
        // rotated ModelTransformation -- see write_tiff's own doc comment
        // for why: nothing downstream reads a ModelTransformation's own
        // rotation/shear terms, so a grivated file must carry no rotation
        // at all rather than one a naive reader would silently drop.
        assert!(decoder
            .find_tag(Tag::Unknown(MODEL_TRANSFORMATION_TAG_FOR_TESTS))
            .unwrap()
            .is_none());
        let expected = geotiff::resample_north_up(
            e2v.width,
            e2v.height,
            origin,
            px_size,
            |local| {
                sample_e2v_bilinear(&e2v, origin, px_size, local)
                    .map(|v| v as f32)
                    .unwrap_or(f32::NAN)
            },
            &georef,
        )
        .unwrap();

        let scale = decoder.get_tag_f64_vec(Tag::Unknown(MODEL_PIXEL_SCALE_TAG)).unwrap();
        assert_eq!(scale, vec![expected.px_size, expected.px_size, 0.0]);

        let tiepoint = decoder.get_tag_f64_vec(Tag::Unknown(MODEL_TIEPOINT_TAG)).unwrap();
        assert_eq!(
            tiepoint,
            vec![0.0, 0.0, 0.0, expected.origin.x, expected.origin.y, 0.0]
        );

        assert_eq!(decoder.dimensions().unwrap(), (expected.width as u32, expected.height as u32));
        assert_eq!(
            decoder.get_tag_ascii_string(Tag::Unknown(GDAL_NODATA_TAG)).unwrap(),
            "nan"
        );
    }

    /// `34264` -- GeoTIFF's own `ModelTransformationTag` number, duplicated
    /// here (rather than importing the module-level constant this file used
    /// to keep) purely so the test above can assert none was written; see
    /// `write_tiff`'s own doc comment for why a rotated transformation tag
    /// is exactly the bug this file no longer writes.
    const MODEL_TRANSFORMATION_TAG_FOR_TESTS: u16 = 34264;

    #[test]
    fn write_tiff_resamples_a_grivated_source_so_no_rotation_survives_to_disk() {
        // The exact regression this module exists to prevent: a grivated
        // map's own DEM, written out and read back by something that (like
        // o-mnia's own custom-GeoTIFF importer) only understands an
        // axis-aligned ModelPixelScale/ModelTiepoint pair, must land at the
        // same place a full rotation-aware reader would put it -- i.e.
        // there must be no rotation left for the naive reader to drop.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevation.tif");
        let georef = Georeferencing {
            scale: 15000,
            epsg: 3006,
            ref_point_x: 322500.0,
            ref_point_y: 6397500.0,
            grivation: 7.1,
            grivation_specified: true,
            auxiliary_scale_factor: 1.000014,
        };
        let origin = c(0.0, 0.0);
        let px_size = 1.0;
        let e2v = ElevationRaster {
            grid: vec![vec![Some(10.0); 20]; 20],
            width: 20,
            height: 20,
        };
        write_tiff(&e2v, origin, px_size, 1.0, Some(&georef), &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(file).unwrap();
        // A naive axis-aligned reader (origin + ModelPixelScale only, no
        // shear) must reconstruct exactly the same footprint a
        // rotation-aware one would -- there is none left to disagree about.
        let scale = decoder.get_tag_f64_vec(Tag::Unknown(MODEL_PIXEL_SCALE_TAG)).unwrap();
        let tiepoint = decoder.get_tag_f64_vec(Tag::Unknown(MODEL_TIEPOINT_TAG)).unwrap();
        assert!(scale[0] > 0.0 && scale[1] > 0.0);
        // The reference point (map origin) must fall strictly inside the
        // axis-aligned bounding box the file declares, confirming the
        // footprint was actually reprojected rather than left centered on
        // some other, unrelated point.
        let (min_x, max_y) = (tiepoint[3], tiepoint[4]);
        let max_x = min_x + scale[0] * decoder.dimensions().unwrap().0 as f64;
        let min_y = max_y - scale[1] * decoder.dimensions().unwrap().1 as f64;
        assert!(min_x <= georef.ref_point_x && georef.ref_point_x <= max_x);
        assert!(min_y <= georef.ref_point_y && georef.ref_point_y <= max_y);
    }
}
