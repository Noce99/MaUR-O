//! Step 5: Final Tiff Computation (`Contours-to-Raster.md`). Builds the
//! Elevation 2D Vector (E2V) -- one elevation value per in-bound Contour
//! Raster (C2V) pixel -- by seeding every contour's own pixels with its own
//! `elevation_height`, running an Elevation Fill Rain Drop Production
//! downhill from every contour to spread a distance-weighted value into
//! every pixel between it and its next lower neighbor
//! ([`crate::step3_rain_drop::elevation_fill_drop_track`]), and filling
//! whatever is left with an iterative 8-connected front propagation. See
//! [`resolve`] for the full pipeline and [`write_tiff`] for turning the
//! result into the actual file.

use std::io::BufWriter;
use std::path::Path;

use geo::Coord;
use tiff::encoder::colortype::Gray32Float;
use tiff::encoder::TiffEncoder;

use crate::contour_raster::{ContourRaster, StepHit, CONTOUR_0_MATRIX_VALUE, OUT_OF_BOUND};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{contour_gravity_side, Contour};
use crate::step3_rain_drop::{elevation_fill_drop_track, placed_sources};

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// One E2V pixel's own accumulated evidence: a running weighted sum and
/// weight total of every value written to it, so the final value is their
/// weighted mean without keeping every individual value around (Step 5). A
/// contour's own seed value and a gap-filling round's own neighbor average
/// each count with weight `1.0`; an Elevation Fill Rain Drop Production's
/// own track instead weighs its value by `1.0 / track_len` -- the shorter a
/// drop's own whole straight track, the more its reading is trusted,
/// specifically so a rare, very long track (an open area with no closer
/// contour along that exact ray) can no longer dominate a pixel some other,
/// much shorter, more locally-relevant track also reached.
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
    /// contour's own `elevation_height` or written to by some Elevation
    /// Fill Rain Drop Production's own track, before gap-filling ran.
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
/// one Elevation Fill Rain Drop Production (Rain direction only -- see the
/// doc's own note on why Anti Rain is not needed here) from every contour
/// with both a gravity direction and an elevation, and fills whatever is
/// left with iterative 8-connected front propagation. Assumes every contour
/// still without an `elevation_height` or a gravity direction has already
/// been dropped/warned about by Steps 3/4; such a contour's own raster
/// footprint is simply skipped as a source (never a sink -- a track can
/// still land on any of its pixels, but only ones that keep some other
/// contour's own real index).
pub fn resolve(contours: &[Contour], raster: &mut ContourRaster, config: &Config) -> Step5Result {
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
        let Some(side) = contour_gravity_side(c) else {
            continue;
        };
        for (source, dir) in placed_sources(&c.lwg.ls, side, config.sources_per_contour_segment) {
            let Some((hit, start, end)) =
                elevation_fill_drop_track(raster, c_idx as u64, source, dir, config)
            else {
                continue; // out of bound: a no-op, per the doc
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
                // same "accordance" assumption Step 4 makes), per the doc's
                // own note on why a high-density hit can no longer be
                // treated as a dead end here.
                StepHit::HighDensity => c_height - 1.0,
                StepHit::OutOfBound => unreachable!(
                    "elevation_fill_drop_track already turns an out-of-bound hit into None"
                ),
            };
            let track_len = dist(start, end);
            if track_len <= 0.0 {
                continue; // a degenerate, zero-length track has nothing to interpolate along
            }
            // A track's own weight is constant along its whole length: the
            // shorter the drop's own straight track, the more every pixel
            // it touches trusts its reading over a longer, less locally
            // relevant one (see `Accum`'s own doc comment on why).
            let weight = 1.0 / track_len;
            for (px, py) in raster.pixels_along_segment(start, end) {
                if px < 0 || py < 0 || px as usize >= width || py as usize >= height {
                    continue;
                }
                let center = raster.pixel_center(px, py);
                let d_a = dist(center, start);
                let d_b = dist(center, end);
                // Weight each end by the distance to the *other* end, not
                // its own: a pixel right next to the start (d_a ~= 0) must
                // land close to eA, so eA's own share of the blend has to
                // grow as d_a shrinks -- i.e. it's carried by d_b, not d_a.
                let value = (d_b * c_height + d_a * hit_height) / (d_a + d_b);
                accum[py as usize][px as usize].add(value, weight);
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

    let grid = accum
        .into_iter()
        .map(|row| row.into_iter().map(|a| a.mean()).collect())
        .collect();

    Step5Result {
        e2v: ElevationRaster { grid, width, height },
        filled_by_rain,
        filled_by_gap_fill: filled_after_gap_fill - filled_by_rain,
        still_undefined: total_in_bound - filled_after_gap_fill,
    }
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

/// Writes `e2v` as a single-band 32-bit float TIFF -- the elevation value,
/// up to a constant (`Contours-to-Raster.md`'s own opening line), for every
/// pixel that has one; `f32::NAN` for a pixel that never got one (always an
/// out-of-bound pixel, only ever a genuinely unreachable in-bound pocket
/// otherwise). No georeferencing tags: nothing else in this crate carries a
/// full georeferenced transform either (see `contour_geometry::meters_per_mm`'s
/// own note on why).
pub fn write_tiff(e2v: &ElevationRaster, path: &Path) -> Result<(), String> {
    let mut data = Vec::with_capacity(e2v.width * e2v.height);
    for y in 0..e2v.height {
        for x in 0..e2v.width {
            data.push(e2v.get(x, y).map(|v| v as f32).unwrap_or(f32::NAN));
        }
    }

    let file =
        std::fs::File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut encoder = TiffEncoder::new(BufWriter::new(file))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let image = encoder
        .new_image::<Gray32Float>(e2v.width as u32, e2v.height as u32)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
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
        }
    }

    fn straight_ls(y: f64) -> LineString<f64> {
        LineString::new(vec![c(0.0, y), c(20.0, y)])
    }

    fn contour_with_gravity(y: f64, gravity_dy: f64, height: f64) -> Contour {
        let mut contour = Contour {
            lwg: LineWithGravity::new(straight_ls(y)),
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

    #[test]
    fn contour_pixels_are_seeded_with_their_own_elevation() {
        let contours = vec![contour_with_gravity(10.0, -1.0, 3.0)];
        let mut raster = raster_for(&contours);
        let result = resolve(&contours, &mut raster, &default_config());

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

        let result = resolve(&contours, &mut raster, &config);

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
        let result = resolve(&contours, &mut raster, &default_config());

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
        let result = resolve(&contours, &mut raster, &config);

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
            elevation_height: Some(5.0),
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(-1.0);
        let contours = vec![contour];
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 40, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        raster.compute_out_of_bound();

        let result = resolve(&contours, &mut raster, &default_config());

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

        let result = resolve(&contours, &mut raster, &default_config());

        let (px, py) = raster.to_px(c(5.0, 5.0)); // dead center of the ring
        assert_eq!(
            result.e2v.get(px as usize, py as usize),
            Some(5.0),
            "the ring's own interior must be reached by gap-filling, at the ring's own height"
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
            elevation_height: Some(5.0),
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(-1.0);
        let contours = vec![contour];
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 40, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        raster.compute_out_of_bound();

        let result = resolve(&contours, &mut raster, &default_config());

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
}
