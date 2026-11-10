//! A DEM benchmark archive on disk: what `create_dem_benchmark` writes and
//! `dem_benchmark` reads (see `mds/DEM-Benchmark-Plan.md`).
//!
//! ```text
//! <archive>/
//!   info.txt                     `key = value` lines: format, seed, equidistance, ...
//!   sample_0001/
//!     map_depressions.omap       contours, with slope lines on depressions
//!     map_none.omap              the same contours, without slope lines
//!     levels.txt                 every line's true height (object index, meters)
//!     ground_truth.tif           the ground model, cut to the sample's box
//!     ground_truth_smoothed.tif  the same, Gaussian-smoothed (see [`SMOOTHING_SIGMA_M`])
//!     water_mask.tif             1 where the ground model is a lake or the sea, else 0
//!     sample.txt                 `key = value` lines: place, tiles, import counts
//!     preview.png                Karttapullautin's own render (optional)
//! ```
//!
//! Besides the layout, the two rasters derived from the ground truth are
//! made here: the smoothed one, which tells the algorithm's error apart from
//! Karttapullautin's generalization, and the water mask, since a lake has
//! no contours in it for anything to be scored on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::dem_geo_tiff_evaluator::GeoRaster;

/// What `info.txt`'s `format` says for this layout.
pub const FORMAT: &str = "dem-benchmark 1";

/// The map variants every sample holds, as named in `map_<variant>.omap`.
pub const VARIANTS: [&str; 2] = ["depressions", "none"];

/// The Gaussian smoothing of `ground_truth_smoothed.tif`, in meters: about
/// the scale Karttapullautin generalizes its contours at.
pub const SMOOTHING_SIGMA_M: f64 = 4.0;

/// How flat (meters, the most a pixel's 3x3 neighbourhood may vary) ground
/// must be to be water. National ground models flatten lakes to a single
/// height; dry ground is never this flat over any area.
pub const WATER_FLATNESS_M: f64 = 0.01;

/// The smallest flat area (square meters) that counts as water.
pub const WATER_MIN_AREA_M2: f64 = 2000.0;

/// An opened archive.
pub struct Archive {
    /// Its folder.
    pub root: PathBuf,
    /// `info.txt`.
    pub info: BTreeMap<String, String>,
    /// Its samples, in name order.
    pub samples: Vec<Sample>,
}

/// One sample's folder.
pub struct Sample {
    /// The folder's name, e.g. `sample_0001`.
    pub name: String,
    /// The folder.
    pub dir: PathBuf,
    /// `sample.txt`.
    pub info: BTreeMap<String, String>,
}

impl Sample {
    /// `map_<variant>.omap`.
    pub fn map(&self, variant: &str) -> PathBuf {
        self.dir.join(format!("map_{variant}.omap"))
    }

    /// `levels.txt`.
    pub fn levels(&self) -> PathBuf {
        self.dir.join("levels.txt")
    }

    /// `ground_truth.tif`.
    pub fn ground_truth(&self) -> PathBuf {
        self.dir.join("ground_truth.tif")
    }

    /// `ground_truth_smoothed.tif`.
    pub fn ground_truth_smoothed(&self) -> PathBuf {
        self.dir.join("ground_truth_smoothed.tif")
    }

    /// `water_mask.tif`.
    pub fn water_mask(&self) -> PathBuf {
        self.dir.join("water_mask.tif")
    }
}

impl Archive {
    /// Opens the archive at `root`: reads `info.txt` (whose `format` must be
    /// [`FORMAT`]) and lists the `sample_*` folders.
    pub fn open(root: &Path) -> Result<Archive, String> {
        let info_path = root.join("info.txt");
        let info = read_key_values(&info_path)?;
        match info.get("format") {
            Some(f) if f == FORMAT => {}
            other => {
                return Err(format!(
                    "{}: not a DEM benchmark archive (format {:?}, expected {FORMAT:?})",
                    info_path.display(),
                    other.map(String::as_str).unwrap_or("missing")
                ))
            }
        }
        let mut samples = Vec::new();
        let entries =
            std::fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read {}: {e}", root.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("sample_") || !entry.path().is_dir() {
                continue;
            }
            let dir = entry.path();
            let info = read_key_values(&dir.join("sample.txt"))?;
            samples.push(Sample { name, dir, info });
        }
        samples.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Archive {
            root: root.to_path_buf(),
            info,
            samples,
        })
    }

    /// The contour interval the maps are drawn at, in meters.
    pub fn equidistance(&self) -> Result<f64, String> {
        self.info
            .get("equidistance_m")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| "info.txt has no equidistance_m".to_string())
    }
}

/// Reads a file of `key = value` lines (blank lines and `#` comments
/// skipped). A key given twice keeps its last value.
pub fn read_key_values(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse_key_values(&text))
}

/// [`read_key_values`] on text.
pub fn parse_key_values(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// `raster` blurred by a Gaussian of `sigma_px` pixels: separable, and
/// no-data aware (a no-data pixel stays one, and its neighbours are
/// averaged over the pixels that have data).
pub fn gaussian_smooth(raster: &GeoRaster, sigma_px: f64) -> GeoRaster {
    let (w, h) = (raster.width, raster.height);
    let radius = (3.0 * sigma_px).ceil() as isize;
    let kernel: Vec<f64> = (-radius..=radius)
        .map(|i| (-(i as f64).powi(2) / (2.0 * sigma_px * sigma_px)).exp())
        .collect();
    let pass = |input: &[f64], horizontal: bool| -> Vec<f64> {
        let mut out = vec![f64::NAN; w * h];
        for y in 0..h {
            for x in 0..w {
                if input[y * w + x].is_nan() {
                    continue;
                }
                let (mut sum, mut weight) = (0.0, 0.0);
                for (k, &kw) in kernel.iter().enumerate() {
                    let d = k as isize - radius;
                    let (sx, sy) = if horizontal {
                        (x as isize + d, y as isize)
                    } else {
                        (x as isize, y as isize + d)
                    };
                    if sx < 0 || sy < 0 || sx >= w as isize || sy >= h as isize {
                        continue;
                    }
                    let v = input[sy as usize * w + sx as usize];
                    if !v.is_nan() {
                        sum += kw * v;
                        weight += kw;
                    }
                }
                out[y * w + x] = sum / weight;
            }
        }
        out
    };
    let values = pass(&pass(&raster.values, true), false);
    GeoRaster {
        values,
        ..raster.clone()
    }
}

/// Which pixels of `raster` are water: those in a connected (4-neighbour)
/// area of at least `min_area_px` pixels whose 3x3 neighbourhoods each vary
/// by at most `flatness`. No-data pixels are not water.
pub fn water_mask(raster: &GeoRaster, flatness: f64, min_area_px: usize) -> Vec<bool> {
    let (w, h) = (raster.width, raster.height);
    let v = &raster.values;
    let mut flat = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let center = v[y * w + x];
            if center.is_nan() {
                continue;
            }
            let (mut lo, mut hi) = (center, center);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let n = v[ny * w + nx];
                    if !n.is_nan() {
                        lo = lo.min(n);
                        hi = hi.max(n);
                    }
                }
            }
            flat[y * w + x] = hi - lo <= flatness;
        }
    }
    let mut water = vec![false; w * h];
    let mut seen = vec![false; w * h];
    let mut stack = Vec::new();
    let mut component = Vec::new();
    for start in 0..w * h {
        if !flat[start] || seen[start] {
            continue;
        }
        component.clear();
        stack.push(start);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            component.push(i);
            let (x, y) = (i % w, i / w);
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ];
            for n in neighbours.into_iter().flatten() {
                if flat[n] && !seen[n] {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        if component.len() >= min_area_px {
            for &i in &component {
                water[i] = true;
            }
        }
    }
    water
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem_geo_tiff_evaluator::{AffineTransform, CrsKey};

    fn raster(width: usize, height: usize, f: impl Fn(usize, usize) -> f64) -> GeoRaster {
        GeoRaster {
            width,
            height,
            values: (0..width * height)
                .map(|i| f(i % width, i / width))
                .collect(),
            transform: AffineTransform {
                origin_x: 0.0,
                origin_y: 0.0,
                col_x: 1.0,
                row_x: 0.0,
                col_y: 0.0,
                row_y: -1.0,
            },
            crs: CrsKey::Projected(3006),
        }
    }

    #[test]
    fn smoothing_keeps_a_plane_and_flattens_a_spike() {
        let plane = raster(20, 20, |x, y| 2.0 * x as f64 + y as f64);
        let smooth = gaussian_smooth(&plane, 2.0);
        // Away from the edges, a plane is its own blur.
        assert!((smooth.values[10 * 20 + 10] - plane.values[10 * 20 + 10]).abs() < 1e-9);

        let spike = raster(21, 21, |x, y| if (x, y) == (10, 10) { 100.0 } else { 0.0 });
        let smooth = gaussian_smooth(&spike, 2.0);
        let peak = smooth.values[10 * 21 + 10];
        assert!(peak > 0.0 && peak < 10.0, "{peak}");
        // Mass is kept away from the edges.
        let total: f64 = smooth.values.iter().sum();
        // (Up to the renormalized kernel where its tail reaches the edge.)
        assert!((total - 100.0).abs() < 0.05, "{total}");
    }

    #[test]
    fn smoothing_leaves_no_data_alone() {
        let r = raster(10, 10, |x, _| if x == 0 { f64::NAN } else { 5.0 });
        let s = gaussian_smooth(&r, 1.5);
        assert!(s.values[0].is_nan());
        assert!((s.values[1] - 5.0).abs() < 1e-12);
    }

    #[test]
    fn a_large_flat_area_is_water_and_a_small_one_is_not() {
        // A 10x10 lake in sloping ground, and a lone flat 2x2 patch.
        let r = raster(30, 30, |x, y| {
            if (5..15).contains(&x) && (5..15).contains(&y) {
                3.0
            } else if (20..22).contains(&x) && (20..22).contains(&y) {
                7.0
            } else {
                0.5 * x as f64 + 0.3 * y as f64 + 10.0
            }
        });
        let water = water_mask(&r, 0.01, 50);
        assert!(water[10 * 30 + 10]);
        // The lake's inner pixels are all flat; its rim, next to sloping
        // ground, is not.
        let count = water.iter().filter(|&&w| w).count();
        assert_eq!(count, 8 * 8);
        assert!(!water[20 * 30 + 20]);
    }

    #[test]
    fn key_values_skip_comments_and_trim() {
        let kv = parse_key_values("# x\nformat = dem-benchmark 1\n\n a=b = c \n");
        assert_eq!(kv["format"], "dem-benchmark 1");
        assert_eq!(kv["a"], "b = c");
    }

    #[test]
    fn an_archive_lists_its_samples_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("info.txt"),
            "format = dem-benchmark 1\nequidistance_m = 5\n",
        )
        .unwrap();
        for name in ["sample_0002", "sample_0001"] {
            let d = dir.path().join(name);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("sample.txt"), "relief_m = 30\n").unwrap();
        }
        std::fs::create_dir(dir.path().join("other")).unwrap();
        let archive = Archive::open(dir.path()).unwrap();
        let names: Vec<_> = archive.samples.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["sample_0001", "sample_0002"]);
        assert_eq!(archive.equidistance().unwrap(), 5.0);
        assert!(archive.samples[0]
            .map("none")
            .ends_with("sample_0001/map_none.omap"));

        std::fs::write(dir.path().join("info.txt"), "format = other\n").unwrap();
        assert!(Archive::open(dir.path()).is_err());
    }
}
