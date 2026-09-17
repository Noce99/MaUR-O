//! Step 5's own per-pixel Gravity Direction prediction -- diagnostic-only for
//! now (see [`crate::contours_to_raster_svg::write_step5_gravity_svg`]), a
//! precursor to [`crate::step5_elevation_raster`]'s per-pixel altitude:
//! instead of a scalar elevation, every in-bound Contour Raster pixel gets a
//! 2D downhill direction, built the same way -- an Elevation Fill Rain Drop
//! Production's own track ([`crate::step3_rain_drop::elevation_fill_drop_track`])
//! seeds every pixel it passes through with its own (constant, since a track
//! is a single straight segment) direction, weighted by `1.0 / track_len^2`
//! -- a steeper distance decay than elevation's own `1.0 / track_len`,
//! since a direction reading has no d_a/d_b interpolation of its own to
//! smooth a long track's reach the way elevation's blend already does, so
//! it leans on its weight alone to favor a short, locally-relevant track
//! over a long one even more aggressively -- and an iterative 8-connected
//! front propagation fills whatever is left. See [`resolve`].

use geo::Coord;

use crate::contour_raster::{ContourRaster, OUT_OF_BOUND};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{contour_gravity_side, Contour};
use crate::step3_rain_drop::{elevation_fill_drop_track, placed_sources};

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// One G2V pixel's own accumulated evidence: a running weighted sum and
/// weight total of every direction vector written to it, so the final value
/// is their weighted mean without keeping every individual reading around --
/// the same role [`crate::step5_elevation_raster`]'s own `Accum` plays for a
/// scalar elevation, just over a 2D vector instead. Unlike elevation, a
/// contour's own pixels get no separate seed value here -- a point sitting
/// exactly on a level line has no gravity direction of its own; it only ever
/// gets one via whichever track(s) happen to start from it, added the same
/// way as every other pixel a track passes through. The final mean is left
/// un-normalized on purpose: its own magnitude (at most `1.0`, since every
/// individual contribution is a unit vector) reflects how much the
/// contributing readings agree, which is exactly what lets a caller tell a
/// confidently-downhill pixel from one where opposing tracks nearly cancel
/// out (see [`GravityRaster::get`]).
#[derive(Clone, Copy, Default)]
struct VecAccum {
    sum: (f64, f64),
    weight_total: f64,
}

impl VecAccum {
    fn add(&mut self, value: (f64, f64), weight: f64) {
        self.sum.0 += value.0 * weight;
        self.sum.1 += value.1 * weight;
        self.weight_total += weight;
    }

    fn mean(&self) -> Option<(f64, f64)> {
        (self.weight_total > 0.0)
            .then(|| (self.sum.0 / self.weight_total, self.sum.1 / self.weight_total))
    }
}

/// The Gravity 2D Vector: one un-normalized downhill direction per Contour
/// Raster pixel, `None` for an out-of-bound pixel or a genuinely unreachable
/// in-bound pocket -- see [`crate::step5_elevation_raster::ElevationRaster`],
/// whose own `None` case means the same thing.
pub struct GravityRaster {
    grid: Vec<Vec<Option<(f64, f64)>>>,
    /// Grid width, in pixels -- the same as the Contour Raster's own.
    pub width: usize,
    /// Grid height, in pixels -- the same as the Contour Raster's own.
    pub height: usize,
}

impl GravityRaster {
    /// The pixel `(x, y)`'s own un-normalized weighted-mean direction, or
    /// `None` if it never got one. Left un-normalized rather than returning a
    /// unit vector -- see [`VecAccum`]'s own doc comment on why the raw
    /// magnitude matters to a caller.
    pub fn get(&self, x: usize, y: usize) -> Option<(f64, f64)> {
        self.grid.get(y).and_then(|row| row.get(x)).copied().flatten()
    }

    /// Builds a [`GravityRaster`] directly from a `grid`, bypassing
    /// [`resolve`] entirely -- for `contours_to_raster_svg`'s own tests of
    /// its gravity-direction drawing layer, which need specific, hand-picked
    /// magnitudes at specific pixels rather than whatever a full Elevation
    /// Fill Rain Drop Production run happens to produce.
    #[cfg(test)]
    pub(crate) fn for_test(
        grid: Vec<Vec<Option<(f64, f64)>>>,
        width: usize,
        height: usize,
    ) -> Self {
        GravityRaster { grid, width, height }
    }
}

/// Runs Step 5's Gravity Direction sub-step: builds the G2V at the same size
/// and pixel grid as `raster`, runs one Elevation Fill Rain Drop Production
/// from every contour with a gravity direction (no `elevation_height`
/// required -- unlike [`crate::step5_elevation_raster::resolve`], a track's
/// own direction needs nothing but the geometry it actually walked), and
/// fills whatever is left with iterative 8-connected front propagation, same
/// as elevation's own gap-filling.
pub fn resolve(contours: &[Contour], raster: &mut ContourRaster, config: &Config) -> GravityRaster {
    let (width, height) = (raster.width, raster.height);
    let mut accum = vec![vec![VecAccum::default(); width]; height];
    let in_bound: Vec<Vec<bool>> = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| raster.get(x as i64, y as i64) != OUT_OF_BOUND)
                .collect()
        })
        .collect();

    for (c_idx, c) in contours.iter().enumerate() {
        let Some(side) = contour_gravity_side(c) else {
            continue;
        };
        for (source, dir) in placed_sources(&c.lwg.ls, side, config.sources_per_contour_segment) {
            let Some((_hit, start, end)) =
                elevation_fill_drop_track(raster, c_idx as u64, source, dir, config)
            else {
                continue; // out of bound: a no-op, same as elevation's own
            };
            let track_len = dist(start, end);
            if track_len <= 0.0 {
                continue; // a degenerate, zero-length track has no direction to give
            }
            // A track's own direction is constant along its whole straight
            // length, so (unlike elevation's own d_a/d_b height blend) every
            // pixel it touches gets the same value; the weight itself uses
            // an inverse-square distance decay instead -- see this module's
            // own doc comment on why that's steeper than elevation's `1.0 /
            // track_len`.
            let direction = ((end.x - start.x) / track_len, (end.y - start.y) / track_len);
            let weight = 1.0 / (track_len * track_len);
            for (px, py) in raster.pixels_along_segment(start, end) {
                if px < 0 || py < 0 || px as usize >= width || py as usize >= height {
                    continue;
                }
                accum[py as usize][px as usize].add(direction, weight);
            }
        }
    }

    // Gap-filling: iterative 8-connected front propagation, Jacobi-style --
    // identical to `step5_elevation_raster::resolve`'s own, just averaging
    // both vector components instead of one scalar.
    loop {
        let mut newly_filled: Vec<(usize, usize, (f64, f64))> = Vec::new();
        for y in 0..height {
            for x in 0..width {
                if !in_bound[y][x] || accum[y][x].mean().is_some() {
                    continue;
                }
                let (mut sum_x, mut sum_y) = (0.0, 0.0);
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
                        if let Some((vx, vy)) = accum[ny as usize][nx as usize].mean() {
                            sum_x += vx;
                            sum_y += vy;
                            count += 1;
                        }
                    }
                }
                if count > 0 {
                    newly_filled.push((x, y, (sum_x / count as f64, sum_y / count as f64)));
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

    let grid: Vec<Vec<Option<(f64, f64)>>> = accum
        .into_iter()
        .map(|row| row.into_iter().map(|a| a.mean()).collect())
        .collect();
    let grid = gaussian_smooth(&grid, config.gravity_gaussian_kernel_size);

    GravityRaster { grid, width, height }
}

/// Gaussian-smooths `grid`'s own direction field: every pixel that already
/// has a direction gets, as its final value, a Gaussian-weighted average of
/// itself and every same-defined neighbor within `kernel_size`'s own window
/// (`config.gravity_gaussian_kernel_size`) -- radius `(kernel_size - 1) / 2`,
/// standard deviation `radius / 3.0` so the window's own edge sits at
/// roughly three standard deviations, normalized by however much of that
/// weight actually landed on a defined neighbor (a pixel near an
/// out-of-bound edge or a still-undefined pocket is averaged over fewer
/// neighbors, not diluted by them reading as zero). A pixel with no
/// direction of its own is left `None` -- this smooths noise among
/// already-resolved pixels; it does not fill new ones (gap-filling above
/// already did that). `kernel_size <= 1` is a no-op, returning `grid`
/// unchanged, since a zero-radius window has nothing else to average with
/// anyway.
fn gaussian_smooth(
    grid: &[Vec<Option<(f64, f64)>>],
    kernel_size: usize,
) -> Vec<Vec<Option<(f64, f64)>>> {
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
            let (mut sum_x, mut sum_y, mut weight_total) = (0.0, 0.0, 0.0);
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx as usize >= width || ny as usize >= height {
                        continue;
                    }
                    let Some((vx, vy)) = grid[ny as usize][nx as usize] else {
                        continue;
                    };
                    let squared_dist = (dx * dx + dy * dy) as f64;
                    let weight = (-squared_dist / (2.0 * sigma * sigma)).exp();
                    sum_x += vx * weight;
                    sum_y += vy * weight;
                    weight_total += weight;
                }
            }
            // `weight_total` is always > 0 here: (dx, dy) = (0, 0) is always
            // in range and always weight 1.0, and `grid[y][x]` (that same
            // pixel) is already known `Some` from the check above.
            smoothed[y][x] = Some((sum_x / weight_total, sum_y / weight_total));
        }
    }
    smoothed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity_model::LineWithGravity;
    use geo::LineString;

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

    fn contour_with_gravity(y: f64, gravity_dy: f64) -> Contour {
        let mut contour = Contour {
            lwg: LineWithGravity::new(straight_ls(y)),
            elevation_height: None,
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(gravity_dy);
        contour
    }

    // Same reasoning as `step5_elevation_raster`'s own `raster_for`: these
    // open, parallel lines don't enclose anything, so `compute_out_of_bound`
    // is deliberately not called here.
    fn raster_for(contours: &[Contour]) -> ContourRaster {
        let mut r = ContourRaster::new(c(-5.0, -20.0), 0.5, 60, 100);
        for (i, contour) in contours.iter().enumerate() {
            r.write_contour(i as u64, &contour.lwg.ls);
        }
        r
    }

    #[test]
    fn a_pixel_between_two_contours_points_from_the_higher_one_toward_the_lower_one() {
        // Two parallel contours 10m apart, downhill = -y -- the same shape
        // `step5_elevation_raster`'s own "midway" test uses, just with
        // neither contour given an `elevation_height` at all: unlike
        // `step5_elevation_raster`, direction needs none.
        let contours = vec![contour_with_gravity(10.0, -1.0), contour_with_gravity(0.0, -1.0)];
        let mut raster = raster_for(&contours);
        let mut config = default_config();
        config.sources_per_contour_segment = 1;
        let result = resolve(&contours, &mut raster, &config);

        let (px, py) = raster.to_px(c(0.0, 5.0));
        let (vx, vy) = result.get(px as usize, py as usize).unwrap();
        let mag = vx.hypot(vy);
        assert!(mag > 0.5, "expected a confidently-downhill vector, got magnitude {mag}");
        assert!(vy < 0.0, "expected the pixel's own direction to point downhill (-y), got {vy}");
    }

    #[test]
    fn out_of_bound_pixels_never_get_a_direction() {
        let ring = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 10.0),
            c(0.0, 10.0),
            c(0.0, 0.0),
        ]);
        let mut contour = Contour {
            lwg: LineWithGravity::new(ring),
            elevation_height: None,
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(-1.0);
        let contours = vec![contour];
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 40, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        raster.compute_out_of_bound();

        let result = resolve(&contours, &mut raster, &default_config());

        assert_eq!(result.get(0, 0), None, "the raster's own border is out of bound");
    }

    #[test]
    fn gap_filling_spreads_a_direction_to_a_pixel_no_track_itself_passed_through() {
        // Same two parallel contours as above, but `sources_per_contour_segment
        // = 1` places exactly one source (at each contour's own x = 0 node),
        // so only the single column of pixels at x = 0 ever gets a direction
        // directly from a track -- a pixel far along the same gap at x = 15
        // has no track of its own and can only ever get a direction from
        // gap-filling spreading it sideways, column by column, same as
        // `step5_elevation_raster`'s own gap-filling test reaches a hilltop's
        // interior no drop's own track could.
        let contours = vec![contour_with_gravity(10.0, -1.0), contour_with_gravity(0.0, -1.0)];
        let mut raster = raster_for(&contours);
        let mut config = default_config();
        config.sources_per_contour_segment = 1;
        let result = resolve(&contours, &mut raster, &config);

        let (px, py) = raster.to_px(c(15.0, 5.0));
        let (vx, vy) = result.get(px as usize, py as usize).unwrap();
        assert!(
            vy < 0.0,
            "expected the gap-filled direction to still point downhill (-y), got ({vx}, {vy})"
        );
    }

    #[test]
    fn gaussian_smooth_is_a_no_op_at_kernel_size_one() {
        let grid = vec![
            vec![Some((1.0, 0.0)), None, Some((0.0, 1.0))],
            vec![None, Some((-1.0, 0.0)), None],
        ];
        assert_eq!(gaussian_smooth(&grid, 1), grid);
        assert_eq!(gaussian_smooth(&grid, 0), grid);
    }

    #[test]
    fn gaussian_smooth_never_gives_a_direction_to_an_undefined_pixel() {
        let grid = vec![vec![Some((1.0, 0.0)), Some((1.0, 0.0)), None]];
        let smoothed = gaussian_smooth(&grid, 5);
        assert_eq!(smoothed[0][2], None, "no pixel of its own to smooth from");
    }

    #[test]
    fn gaussian_smooth_is_unaffected_by_a_wholly_undefined_neighborhood() {
        // A single defined pixel with nothing but `None` all around it: its
        // own weight (distance 0, weight 1.0) is the *only* contribution to
        // its own weighted average, so it must come back completely
        // unchanged, not diluted toward zero by its absent neighbors.
        let mut grid = vec![vec![None; 5]; 5];
        grid[2][2] = Some((3.0, -4.0));
        let smoothed = gaussian_smooth(&grid, 5);
        assert_eq!(smoothed[2][2], Some((3.0, -4.0)));
    }

    #[test]
    fn gaussian_smooth_blends_toward_a_defined_neighbors_own_direction() {
        // Two side-by-side defined pixels pointing opposite ways: each
        // pixel's own smoothed direction must lean toward its neighbor's
        // (pulling its own x component off of its un-smoothed extreme, here
        // 1.0/-1.0), without fully reaching it (its own, still-largest
        // weight keeps it closer to its own original value than to the
        // neighbor's).
        let grid = vec![vec![Some((1.0, 0.0)), Some((-1.0, 0.0))]];
        let smoothed = gaussian_smooth(&grid, 5);
        let (sx0, _) = smoothed[0][0].unwrap();
        let (sx1, _) = smoothed[0][1].unwrap();
        assert!(
            sx0 < 1.0 && sx0 > 0.0,
            "expected pixel 0 to lean toward its neighbor's -1.0 without reaching it, got {sx0}"
        );
        assert!(
            sx1 > -1.0 && sx1 < 0.0,
            "expected pixel 1 to lean toward its neighbor's 1.0 without reaching it, got {sx1}"
        );
    }
}
