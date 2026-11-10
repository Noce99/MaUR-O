//! Scoring `contours_to_raster` on one DEM benchmark sample (see
//! [`crate::dem_archive`] and `mds/DEM-Benchmark-Plan.md`): how far its
//! elevation model is from the ground model the contours were drawn from.
//!
//! RMSE alone hides the failure that matters most. The algorithm has to
//! infer every contour's level from the shapes alone; a level wrong by one
//! band, or a hill read as a pit, is a different kind of error from the
//! interpolation between correct levels, which is bounded by about one
//! equidistance and partly unavoidable. So beside the pixel errors,
//! [`score`] measures:
//!
//! - **per-contour level accuracy**: each line's predicted level (the median
//!   of the prediction along it) against its true one (the height
//!   Karttapullautin drew it at), after the one whole-band offset that
//!   agrees with most of the contour length;
//! - **inverted area**: the share of sloping ground whose predicted slope
//!   points the other way from the true one;
//! - **a baseline**: the same lines at their *true* levels, filled in by
//!   Laplace interpolation ([`laplace_baseline`]) -- about the best any
//!   interpolation of these contours does, so the gap between it and the
//!   prediction is the cost of having had to infer the levels.
//!
//! An elevation model is only meaningful up to an additive constant, so
//! every pixel error is measured after removing one: the mean difference
//! (what least squares picks, and what `dem_geo_tiff_evaluator` uses) and,
//! separately, the median, which a single inverted hill cannot drag off.

use geo::Coord;

use crate::dem_from_contours::DemFromContoursResult;
use crate::dem_geo_tiff_evaluator::{bilinear_sample, AffineTransform, CrsKey, GeoRaster};
use crate::geotiff::{ground_to_projected, resample_north_up};
use crate::map::{Map, ObjectKind};

/// The least slope (rise over run) both models must have at a pixel for its
/// direction to count towards the inverted area: below it, the direction of
/// near-flat ground is noise.
pub const MIN_SLOPE: f64 = 0.05;

/// How far apart (meters) a line is sampled for its predicted level.
const LINE_SAMPLE_STEP: f64 = 1.0;

/// One line of the map, on the ground, with its true level.
#[derive(Clone, Debug)]
pub struct Line {
    /// Its vertices in the map's projected CRS.
    pub points: Vec<(f64, f64)>,
    /// Its true height, meters.
    pub level: f64,
}

impl Line {
    /// Its length, meters.
    pub fn length(&self) -> f64 {
        self.points
            .windows(2)
            .map(|s| (s[1].0 - s[0].0).hypot(s[1].1 - s[0].1))
            .sum()
    }
}

/// The map's line objects that `levels` gives a level for (by object
/// index, as `levels.txt` lists them), placed on the ground by the map's
/// georeferencing.
pub fn map_lines(map: &Map, levels: &[(usize, f64)]) -> Result<Vec<Line>, String> {
    let georef = map
        .georeferencing
        .as_ref()
        .ok_or("the map is not georeferenced")?;
    let meters_per_mm = f64::from(georef.scale) / 1000.0;
    let mut lines = Vec::new();
    for &(index, level) in levels {
        let object = map
            .objects
            .get(index)
            .ok_or_else(|| format!("levels name object {index}, which the map does not have"))?;
        if !matches!(object.kind, ObjectKind::Path(_)) {
            return Err(format!("levels name object {index}, which is not a line"));
        }
        let points = object
            .coords
            .iter()
            .filter_map(|c| {
                ground_to_projected(
                    georef,
                    Coord {
                        x: c.x * meters_per_mm,
                        y: c.y * meters_per_mm,
                    },
                )
                .map(|p| (p.x, p.y))
            })
            .collect();
        lines.push(Line { points, level });
    }
    Ok(lines)
}

/// `contours_to_raster`'s elevation grid placed on the ground: resampled
/// north-up in the map's projected CRS, the shape a ground truth GeoTIFF is
/// in.
pub fn prediction_raster(result: &DemFromContoursResult, map: &Map) -> Result<GeoRaster, String> {
    let georef = map
        .georeferencing
        .as_ref()
        .ok_or("the map is not georeferenced")?;
    let origin = Coord {
        x: result.origin_ground.0,
        y: result.origin_ground.1,
    };
    let px = result.px_size_m;
    let (w, h) = (result.width, result.height);
    // Bilinear over the grid's pixel centers, NaN where any corner is.
    let sample = |local: Coord<f64>| -> f32 {
        let fx = (local.x - origin.x) / px - 0.5;
        let fy = (local.y - origin.y) / px - 0.5;
        if fx < -0.5 || fy < -0.5 || fx > w as f64 - 0.5 || fy > h as f64 - 0.5 {
            return f32::NAN;
        }
        let x0 = fx.floor().clamp(0.0, (w - 1) as f64);
        let y0 = fy.floor().clamp(0.0, (h - 1) as f64);
        let (tx, ty) = ((fx - x0).clamp(0.0, 1.0), (fy - y0).clamp(0.0, 1.0));
        let (x0, y0) = (x0 as usize, y0 as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let v = |x: usize, y: usize| f64::from(result.elev[y * w + x]);
        let top = v(x0, y0) * (1.0 - tx) + v(x1, y0) * tx;
        let bottom = v(x0, y1) * (1.0 - tx) + v(x1, y1) * tx;
        (top * (1.0 - ty) + bottom * ty) as f32
    };
    let north_up = resample_north_up(w, h, origin, px, sample, georef)
        .ok_or("the map's georeferencing names no CRS")?;
    Ok(GeoRaster {
        width: north_up.width,
        height: north_up.height,
        values: north_up.data.iter().map(|&v| f64::from(v)).collect(),
        transform: AffineTransform {
            origin_x: north_up.origin.x,
            origin_y: north_up.origin.y,
            col_x: north_up.px_size,
            row_x: 0.0,
            col_y: 0.0,
            row_y: -north_up.px_size,
        },
        crs: u32::try_from(georef.epsg)
            .map(CrsKey::Projected)
            .unwrap_or(CrsKey::Unspecified),
    })
}

/// Pixel errors after removing one additive constant.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PixelErrors {
    /// The constant added to the prediction.
    pub offset: f64,
    /// Root mean square error, meters.
    pub rmse: f64,
    /// Mean absolute error, meters.
    pub mae: f64,
}

/// Per-contour level accuracy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LevelAccuracy {
    /// Lines scored: those with a prediction along them.
    pub lines: usize,
    /// The whole-band offset (in equidistances) removed before comparing.
    pub band_offset: i64,
    /// Share of lines at exactly the right level, `[0, 1]`.
    pub exact: f64,
    /// Share of lines one band off.
    pub off_by_one: f64,
    /// Share of lines more than one band off.
    pub off_by_more: f64,
    /// Share of the scored lines' length at a wrong level.
    pub wrong_length: f64,
}

/// Everything [`score`] measures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scores {
    /// Pixels scored (with data on both sides, not water, inside the margin).
    pub pixels: usize,
    /// After removing the mean difference.
    pub mean_offset: PixelErrors,
    /// After removing the median difference.
    pub median_offset: PixelErrors,
    /// Per-contour level accuracy.
    pub levels: LevelAccuracy,
    /// Share of sloping pixels whose predicted slope points away from the
    /// true one (more than 90° off), `[0, 1]`.
    pub inverted: f64,
    /// The Laplace baseline's errors, after removing its mean difference.
    pub baseline: PixelErrors,
}

/// Scores `prediction` against `truth` (resampled onto its grid) over the
/// pixels with data in both, not `water`, and at least `margin_m` from the
/// edge of `truth`. `lines` (with their true levels) are what the
/// per-contour accuracy and the baseline are made of.
pub fn score(
    truth: &GeoRaster,
    prediction: &GeoRaster,
    water: Option<&[bool]>,
    lines: &[Line],
    equidistance: f64,
    margin_m: f64,
) -> Result<Scores, String> {
    let (w, h) = (truth.width, truth.height);
    let px = truth.transform.col_x.abs();
    let margin = (margin_m / px).ceil() as usize;
    let center = |col: usize, row: usize| {
        truth
            .transform
            .pixel_to_world(col as f64 + 0.5, row as f64 + 0.5)
    };

    // The prediction on the truth's grid.
    let mut predicted = vec![f64::NAN; w * h];
    for row in 0..h {
        for col in 0..w {
            let (x, y) = center(col, row);
            if let Some(v) = bilinear_sample(prediction, x, y) {
                predicted[row * w + col] = v;
            }
        }
    }
    let inside = |col: usize, row: usize| {
        col >= margin && row >= margin && col + margin < w && row + margin < h
    };
    let scored = |i: usize| {
        let (col, row) = (i % w, i / w);
        inside(col, row)
            && !truth.values[i].is_nan()
            && !predicted[i].is_nan()
            && water.is_none_or(|m| !m[i])
    };
    let diffs: Vec<f64> = (0..w * h)
        .filter(|&i| scored(i))
        .map(|i| truth.values[i] - predicted[i])
        .collect();
    if diffs.is_empty() {
        return Err("no pixel has data in both the ground truth and the prediction".to_string());
    }

    let errors = |offset: f64| {
        let n = diffs.len() as f64;
        let mse = diffs.iter().map(|d| (d - offset).powi(2)).sum::<f64>() / n;
        let mae = diffs.iter().map(|d| (d - offset).abs()).sum::<f64>() / n;
        PixelErrors {
            offset,
            rmse: mse.sqrt(),
            mae,
        }
    };
    let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let mean_offset = errors(mean);
    let median_offset = errors(median(diffs.clone()));

    // Slope directions, by central differences on the truth's grid.
    let gradient = |values: &[f64], col: usize, row: usize| -> Option<(f64, f64)> {
        if col == 0 || row == 0 || col + 1 >= w || row + 1 >= h {
            return None;
        }
        let v = |c: usize, r: usize| Some(values[r * w + c]).filter(|v| !v.is_nan());
        let gx = (v(col + 1, row)? - v(col - 1, row)?) / (2.0 * px);
        let gy = (v(col, row - 1)? - v(col, row + 1)?) / (2.0 * px);
        Some((gx, gy))
    };
    let (mut sloping, mut inverted) = (0usize, 0usize);
    for i in (0..w * h).filter(|&i| scored(i)) {
        let (col, row) = (i % w, i / w);
        let (Some(t), Some(p)) = (
            gradient(&truth.values, col, row),
            gradient(&predicted, col, row),
        ) else {
            continue;
        };
        if t.0.hypot(t.1) < MIN_SLOPE || p.0.hypot(p.1) < MIN_SLOPE {
            continue;
        }
        sloping += 1;
        if t.0 * p.0 + t.1 * p.1 < 0.0 {
            inverted += 1;
        }
    }

    let levels = level_accuracy(prediction, lines, equidistance);

    let baseline_raster = laplace_baseline(truth, lines);
    let baseline_diffs: Vec<f64> = (0..w * h)
        .filter(|&i| scored(i) && !baseline_raster.values[i].is_nan())
        .map(|i| truth.values[i] - baseline_raster.values[i])
        .collect();
    let baseline = if baseline_diffs.is_empty() {
        PixelErrors::default()
    } else {
        let n = baseline_diffs.len() as f64;
        let offset = baseline_diffs.iter().sum::<f64>() / n;
        PixelErrors {
            offset,
            rmse: (baseline_diffs
                .iter()
                .map(|d| (d - offset).powi(2))
                .sum::<f64>()
                / n)
                .sqrt(),
            mae: baseline_diffs
                .iter()
                .map(|d| (d - offset).abs())
                .sum::<f64>()
                / n,
        }
    };

    Ok(Scores {
        pixels: diffs.len(),
        mean_offset,
        median_offset,
        levels,
        inverted: if sloping == 0 {
            0.0
        } else {
            inverted as f64 / sloping as f64
        },
        baseline,
    })
}

/// The median of `values` (the mean of the middle two for an even count).
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Points every [`LINE_SAMPLE_STEP`] meters along `line`, its vertices
/// included.
fn along(line: &Line) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for s in line.points.windows(2) {
        let (a, b) = (s[0], s[1]);
        let length = (b.0 - a.0).hypot(b.1 - a.1);
        let steps = (length / LINE_SAMPLE_STEP).ceil().max(1.0) as usize;
        for k in 0..steps {
            let t = k as f64 / steps as f64;
            out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    if let Some(&last) = line.points.last() {
        out.push(last);
    }
    out
}

/// Each line's predicted level against its true one, in whole bands, after
/// the band offset that agrees with the most line length.
pub fn level_accuracy(prediction: &GeoRaster, lines: &[Line], equidistance: f64) -> LevelAccuracy {
    // (band difference, length) per line with a prediction along it.
    let mut scored: Vec<(f64, f64)> = Vec::new();
    for line in lines {
        let values: Vec<f64> = along(line)
            .into_iter()
            .filter_map(|(x, y)| bilinear_sample(prediction, x, y))
            .collect();
        if values.is_empty() {
            continue;
        }
        let predicted = median(values);
        scored.push(((predicted - line.level) / equidistance, line.length()));
    }
    if scored.is_empty() {
        return LevelAccuracy::default();
    }
    // The offset with the most length behind it.
    let mut by_offset: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    for &(d, length) in &scored {
        *by_offset.entry(d.round() as i64).or_default() += length;
    }
    let band_offset = by_offset
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(&k, _)| k)
        .unwrap_or(0);
    let (mut exact, mut one, mut more) = (0usize, 0usize, 0usize);
    let (mut total_length, mut wrong_length) = (0.0, 0.0);
    for &(d, length) in &scored {
        let error = (d - band_offset as f64).round().abs() as i64;
        match error {
            0 => exact += 1,
            1 => one += 1,
            _ => more += 1,
        }
        total_length += length;
        if error != 0 {
            wrong_length += length;
        }
    }
    let n = scored.len() as f64;
    LevelAccuracy {
        lines: scored.len(),
        band_offset,
        exact: exact as f64 / n,
        off_by_one: one as f64 / n,
        off_by_more: more as f64 / n,
        wrong_length: if total_length > 0.0 {
            wrong_length / total_length
        } else {
            0.0
        },
    }
}

/// The `lines` at their true levels, drawn onto `grid`'s pixels and the
/// rest filled in by Laplace interpolation (each free pixel the mean of its
/// four neighbours; the grid's edge is a mirror, so ground past the last
/// contour levels off). `NaN` everywhere if no line touches the grid.
pub fn laplace_baseline(grid: &GeoRaster, lines: &[Line]) -> GeoRaster {
    let (w, h) = (grid.width, grid.height);
    let mut values = vec![f64::NAN; w * h];
    let mut fixed = vec![false; w * h];
    let step = grid.transform.col_x.abs() / 4.0;
    for line in lines {
        for s in line.points.windows(2) {
            let (a, b) = (s[0], s[1]);
            let n = ((b.0 - a.0).hypot(b.1 - a.1) / step).ceil().max(1.0) as usize;
            for k in 0..=n {
                let t = k as f64 / n as f64;
                let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                let (c, r) = grid.transform.world_to_pixel(x, y);
                if c < 0.0 || r < 0.0 || c >= w as f64 || r >= h as f64 {
                    continue;
                }
                let i = r as usize * w + c as usize;
                values[i] = line.level;
                fixed[i] = true;
            }
        }
    }
    if !fixed.iter().any(|&f| f) {
        return GeoRaster {
            values,
            ..grid.clone()
        };
    }

    // Start every free pixel from its nearest fixed one (breadth first),
    // which leaves relaxation little to do.
    let mut queue: std::collections::VecDeque<usize> = (0..w * h).filter(|&i| fixed[i]).collect();
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        for n in [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then(|| i + 1),
            (y > 0).then(|| i - w),
            (y + 1 < h).then(|| i + w),
        ]
        .into_iter()
        .flatten()
        {
            if values[n].is_nan() {
                values[n] = values[i];
                queue.push_back(n);
            }
        }
    }

    // Successive over-relaxation until nothing moves by a millimetre.
    const OMEGA: f64 = 1.9;
    for _ in 0..2000 {
        let mut largest: f64 = 0.0;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if fixed[i] {
                    continue;
                }
                let left = values[if x > 0 { i - 1 } else { i + 1 }];
                let right = values[if x + 1 < w { i + 1 } else { i - 1 }];
                let up = values[if y > 0 { i - w } else { i + w }];
                let down = values[if y + 1 < h { i + w } else { i - w }];
                let target = (left + right + up + down) / 4.0;
                let change = OMEGA * (target - values[i]);
                values[i] += change;
                largest = largest.max(change.abs());
            }
        }
        if largest < 1e-3 {
            break;
        }
    }
    GeoRaster {
        values,
        ..grid.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: usize, height: usize, f: impl Fn(f64, f64) -> f64) -> GeoRaster {
        let transform = AffineTransform {
            origin_x: 0.0,
            origin_y: height as f64,
            col_x: 1.0,
            row_x: 0.0,
            col_y: 0.0,
            row_y: -1.0,
        };
        let values = (0..width * height)
            .map(|i| {
                let (x, y) =
                    transform.pixel_to_world((i % width) as f64 + 0.5, (i / width) as f64 + 0.5);
                f(x, y)
            })
            .collect();
        GeoRaster {
            width,
            height,
            values,
            transform,
            crs: CrsKey::Projected(3006),
        }
    }

    /// Vertical contour lines every `spacing` meters on a ramp rising
    /// `rise` per meter eastwards.
    fn ramp_lines(width: f64, height: f64, spacing: f64, rise: f64) -> Vec<Line> {
        let mut lines = Vec::new();
        let mut x = spacing;
        while x < width {
            lines.push(Line {
                points: vec![(x, 0.0), (x, height)],
                level: x * rise,
            });
            x += spacing;
        }
        lines
    }

    #[test]
    fn a_shifted_copy_scores_perfectly() {
        let truth = grid(60, 40, |x, y| 0.5 * x + 0.1 * y);
        let prediction = grid(60, 40, |x, y| 0.5 * x + 0.1 * y - 123.0);
        let lines = ramp_lines(60.0, 40.0, 10.0, 0.5);
        let s = score(&truth, &prediction, None, &[], 5.0, 2.0).unwrap();
        assert!(s.mean_offset.rmse < 1e-9 && s.median_offset.mae < 1e-9);
        assert!((s.mean_offset.offset - 123.0).abs() < 1e-9);
        assert_eq!(s.inverted, 0.0);

        let acc = level_accuracy(&prediction, &lines, 5.0);
        assert_eq!(acc.lines, 5);
        assert_eq!(acc.exact, 1.0);
        // Along a line the plane still rises 0.1 m per meter north, so its
        // median is 2 m above the line's level: -121 m, -24.2 bands, whose
        // whole part is the offset.
        assert_eq!(acc.band_offset, -24);
    }

    #[test]
    fn an_upside_down_prediction_is_all_inverted() {
        let truth = grid(40, 40, |x, _| x);
        let prediction = grid(40, 40, |x, _| -x);
        let s = score(&truth, &prediction, None, &[], 5.0, 2.0).unwrap();
        assert!((s.inverted - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_line_off_by_a_band_is_counted() {
        let lines = ramp_lines(60.0, 40.0, 10.0, 0.5);
        let mut wrong = lines.clone();
        wrong[2].level += 5.0;
        let truth = grid(60, 40, |x, _| 0.5 * x);
        let acc = level_accuracy(&truth, &wrong, 5.0);
        assert_eq!(acc.lines, 5);
        assert!((acc.exact - 0.8).abs() < 1e-12);
        assert!((acc.off_by_one - 0.2).abs() < 1e-12);
        assert!((acc.wrong_length - 0.2).abs() < 1e-12);
    }

    #[test]
    fn the_baseline_rebuilds_a_ramp_between_its_contours() {
        let truth = grid(60, 40, |x, _| 0.5 * x);
        let lines = ramp_lines(60.0, 40.0, 10.0, 0.5);
        let baseline = laplace_baseline(&truth, &lines);
        // Between the first and last line the ramp is linear, which Laplace
        // reproduces up to where the line falls inside its pixel.
        for col in 12..48 {
            let v = baseline.values[20 * 60 + col];
            assert!(
                (v - truth.values[20 * 60 + col]).abs() < 0.3,
                "col {col}: {v}"
            );
        }
        let s = score(&truth, &truth, None, &lines, 5.0, 12.0).unwrap();
        assert!(s.baseline.rmse < 0.3, "{:?}", s.baseline);
    }

    #[test]
    fn water_is_not_scored() {
        let truth = grid(20, 20, |x, _| x);
        let prediction = grid(20, 20, |x, _| if x < 10.0 { x } else { 0.0 });
        let water: Vec<bool> = (0..400).map(|i| i % 20 >= 10).collect();
        let s = score(&truth, &prediction, Some(&water), &[], 5.0, 0.0).unwrap();
        assert_eq!(s.pixels, 200);
        assert!(s.mean_offset.rmse < 1e-9);
    }

    #[test]
    fn the_median_offset_ignores_a_minority_far_off() {
        let truth = grid(20, 20, |_, _| 0.0);
        let prediction = grid(20, 20, |x, _| if x < 4.0 { 50.0 } else { 10.0 });
        let s = score(&truth, &prediction, None, &[], 5.0, 0.0).unwrap();
        assert!((s.median_offset.offset + 10.0).abs() < 1e-9);
        assert!(s.median_offset.mae < s.mean_offset.mae);
    }
}
