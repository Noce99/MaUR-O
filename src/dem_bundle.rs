//! A DEM benchmark run as one zip file, for a viewer to read (see
//! `mds/DEM-Benchmark-Viewer-Plan.md`).
//!
//! `dem_benchmark`'s own results are a text table and three PNGs per
//! sample: enough to rank, not to see why a sample fails. The bundle holds
//! what a 3D comparison and its charts need, in shapes a browser reads
//! without a GeoTIFF decoder:
//!
//! ```text
//! bundle.zip
//!   results.json                run info, means per variant, per-sample metrics
//!   sample_0001/
//!     sample.json               the sample's own facts, its grid, per-variant metrics
//!     truth.f32                 the ground model on the grid
//!     water.u8                  the water mask on the grid (1 = water)
//!     <variant>/
//!       prediction.f32          contours_to_raster's model on the grid, on the truth's datum
//!       lines.json              every contour: points, true and predicted level, error
//! ```
//!
//! Every grid of a sample is the same [`Grid`]: the ground truth's box in
//! [`BUNDLE_CELL_M`] cells, north-up in the sample's projected CRS, so the
//! truth, the prediction and the water line up cell for cell. Grids are raw
//! little-endian (`f32`, `NaN` for no data; `u8`), row-major, top row first.
//! A prediction's heights are only defined up to a constant; the bundle
//! adds the one the scores remove (the mean difference from the truth), so
//! a prediction and its lines' predicted levels read as heights above sea.
//!
//! The JSON is written by hand ([`Obj`], [`array()`]): its shapes are fixed
//! and small, and the crate has no serde.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::dem_benchmark::{LevelAccuracy, Line, PixelErrors, Scores};
use crate::dem_geo_tiff_evaluator::{bilinear_sample, GeoRaster};

/// What `results.json`'s `format` says, so a reader can refuse a bundle it
/// does not know.
pub const BUNDLE_FORMAT: &str = "dem-benchmark-bundle 1";

/// The bundle's cell size, meters: the 1 m ground model at half resolution,
/// about 1 MB per grid for a 1 km sample.
pub const BUNDLE_CELL_M: f64 = 2.0;

/// The grid every raster of one sample is on: north-up, `cell` meters
/// square, its top-left corner at `(origin_x, origin_y)` in `epsg`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// The left edge, in the projected CRS.
    pub origin_x: f64,
    /// The top edge, in the projected CRS.
    pub origin_y: f64,
    /// Cell size, meters.
    pub cell: f64,
    /// Columns.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// The projected CRS.
    pub epsg: u32,
}

impl Grid {
    /// The box `raster` covers, in `cell`-meter cells (the last row and
    /// column cut short if its size is not a multiple of `cell`). `raster`
    /// must be north-up.
    pub fn covering(raster: &GeoRaster, cell: f64, epsg: u32) -> Grid {
        let t = &raster.transform;
        let ground_w = raster.width as f64 * t.col_x.abs();
        let ground_h = raster.height as f64 * t.row_y.abs();
        Grid {
            origin_x: t.origin_x,
            origin_y: t.origin_y,
            cell,
            width: (ground_w / cell).round().max(1.0) as usize,
            height: (ground_h / cell).round().max(1.0) as usize,
            epsg,
        }
    }

    /// The center of cell `(col, row)`, in the projected CRS.
    pub fn center(&self, col: usize, row: usize) -> (f64, f64) {
        (
            self.origin_x + (col as f64 + 0.5) * self.cell,
            self.origin_y - (row as f64 + 0.5) * self.cell,
        )
    }

    /// The cell `(x, y)` falls in, if any.
    pub fn cell_of(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        let col = ((x - self.origin_x) / self.cell).floor();
        let row = ((self.origin_y - y) / self.cell).floor();
        if col < 0.0 || row < 0.0 || col >= self.width as f64 || row >= self.height as f64 {
            return None;
        }
        Some((col as usize, row as usize))
    }

    /// The grid as a JSON object.
    pub fn to_json(&self) -> String {
        Obj::new()
            .num("origin_x", self.origin_x)
            .num("origin_y", self.origin_y)
            .num("cell_m", self.cell)
            .int("width", self.width as i64)
            .int("height", self.height as i64)
            .int("epsg", i64::from(self.epsg))
            .build()
    }
}

/// `raster` averaged onto `grid`: each cell the mean of the pixels whose
/// centers fall in it, `NaN` where none has data. For a ground truth finer
/// than the grid.
pub fn block_mean(raster: &GeoRaster, grid: &Grid) -> Vec<f32> {
    let mut sum = vec![0.0f64; grid.width * grid.height];
    let mut count = vec![0u32; grid.width * grid.height];
    for row in 0..raster.height {
        for col in 0..raster.width {
            let v = raster.values[row * raster.width + col];
            if v.is_nan() {
                continue;
            }
            let (x, y) = raster
                .transform
                .pixel_to_world(col as f64 + 0.5, row as f64 + 0.5);
            if let Some((c, r)) = grid.cell_of(x, y) {
                sum[r * grid.width + c] += v;
                count[r * grid.width + c] += 1;
            }
        }
    }
    sum.iter()
        .zip(&count)
        .map(|(&s, &n)| {
            if n == 0 {
                f32::NAN
            } else {
                (s / n as f64) as f32
            }
        })
        .collect()
}

/// `raster` sampled bilinearly at `grid`'s cell centers, plus `offset`,
/// `NaN` where it has no data. For a prediction on a grid of its own,
/// `offset` putting it on the ground truth's datum.
pub fn sample_onto(raster: &GeoRaster, grid: &Grid, offset: f64) -> Vec<f32> {
    let mut out = Vec::with_capacity(grid.width * grid.height);
    for row in 0..grid.height {
        for col in 0..grid.width {
            let (x, y) = grid.center(col, row);
            out.push(bilinear_sample(raster, x, y).map_or(f32::NAN, |v| (v + offset) as f32));
        }
    }
    out
}

/// A mask on `raster`'s pixels (true = water) onto `grid`: a cell is water
/// when most of the pixels in it are.
pub fn mask_onto(mask: &[bool], raster: &GeoRaster, grid: &Grid) -> Vec<u8> {
    let mut water = vec![0u32; grid.width * grid.height];
    let mut count = vec![0u32; grid.width * grid.height];
    for row in 0..raster.height {
        for col in 0..raster.width {
            let (x, y) = raster
                .transform
                .pixel_to_world(col as f64 + 0.5, row as f64 + 0.5);
            if let Some((c, r)) = grid.cell_of(x, y) {
                count[r * grid.width + c] += 1;
                if mask.get(row * raster.width + col) == Some(&true) {
                    water[r * grid.width + c] += 1;
                }
            }
        }
    }
    water
        .iter()
        .zip(&count)
        .map(|(&w, &n)| u8::from(n > 0 && 2 * w > n))
        .collect()
}

/// `values` as little-endian bytes.
pub fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// `lines.json`: one object per line, `{"level", "predicted", "error",
/// "length_m", "points"}`, `predicted` and `error` (whole bands, after the
/// band offset) `null` where the prediction does not reach the line, points
/// rounded to 0.1 m. `offset` is added to every predicted level: the
/// constant that puts the prediction on the ground truth's datum.
pub fn lines_json(
    lines: &[Line],
    levels: &LevelAccuracy,
    equidistance: f64,
    offset: f64,
) -> String {
    array(lines.iter().enumerate().map(|(i, line)| {
        let points = array(
            line.points
                .iter()
                .map(|&(x, y)| format!("[{},{}]", round1(x), round1(y))),
        );
        Obj::new()
            .num("level", line.level)
            .opt_num(
                "predicted",
                levels
                    .predicted
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|p| round2(p + offset)),
            )
            .opt_int("error", levels.band_error(i, line.level, equidistance))
            .num("length_m", round1(line.length()))
            .raw("points", points)
            .build()
    }))
}

/// Every number of `scores` as a JSON object, `equidistance` for the RMSE
/// in equidistances.
pub fn scores_json(scores: &Scores, equidistance: f64) -> String {
    let errors = |e: &PixelErrors| {
        Obj::new()
            .num("offset_m", e.offset)
            .num("rmse_m", e.rmse)
            .num("mae_m", e.mae)
            .build()
    };
    let l = &scores.levels;
    Obj::new()
        .int("pixels", scores.pixels as i64)
        .raw("mean_offset", errors(&scores.mean_offset))
        .raw("median_offset", errors(&scores.median_offset))
        .num("rmse_equidistances", scores.mean_offset.rmse / equidistance)
        .raw(
            "levels",
            Obj::new()
                .int("lines", l.lines as i64)
                .int("band_offset", l.band_offset)
                .num("exact", l.exact)
                .num("off_by_one", l.off_by_one)
                .num("off_by_more", l.off_by_more)
                .num("wrong_length", l.wrong_length)
                .build(),
        )
        .num("inverted", scores.inverted)
        .raw("baseline", errors(&scores.baseline))
        .build()
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// A JSON number: `null` for `NaN` and infinities, which JSON has no
/// spelling for.
pub fn number(v: f64) -> String {
    if v.is_finite() {
        // Shortest round-trip form; integers without a trailing ".0".
        let s = format!("{v}");
        s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
    } else {
        "null".to_string()
    }
}

/// A JSON string literal.
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON array of already-written values.
pub fn array(items: impl IntoIterator<Item = String>) -> String {
    let mut out = String::from("[");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&item);
    }
    out.push(']');
    out
}

/// A JSON object, built one member at a time, in order.
#[derive(Default)]
pub struct Obj(String);

impl Obj {
    /// An empty object.
    pub fn new() -> Obj {
        Obj::default()
    }

    /// Adds `key` with an already-written JSON value.
    pub fn raw(mut self, key: &str, json: String) -> Obj {
        if !self.0.is_empty() {
            self.0.push(',');
        }
        self.0.push_str(&string(key));
        self.0.push(':');
        self.0.push_str(&json);
        self
    }

    /// Adds a number (`null` if not finite).
    pub fn num(self, key: &str, v: f64) -> Obj {
        self.raw(key, number(v))
    }

    /// Adds a number or `null`.
    pub fn opt_num(self, key: &str, v: Option<f64>) -> Obj {
        self.raw(key, v.map_or("null".to_string(), number))
    }

    /// Adds an integer.
    pub fn int(self, key: &str, v: i64) -> Obj {
        self.raw(key, v.to_string())
    }

    /// Adds an integer or `null`.
    pub fn opt_int(self, key: &str, v: Option<i64>) -> Obj {
        self.raw(key, v.map_or("null".to_string(), |v| v.to_string()))
    }

    /// Adds a string.
    pub fn str(self, key: &str, v: &str) -> Obj {
        self.raw(key, string(v))
    }

    /// The object's JSON text.
    pub fn build(self) -> String {
        format!("{{{}}}", self.0)
    }
}

/// A bundle being written: entries are deflated into the zip as they are
/// added.
pub struct BundleWriter {
    zip: zip::ZipWriter<BufWriter<File>>,
}

impl BundleWriter {
    /// Starts a bundle at `path`, replacing any file there.
    pub fn create(path: &Path) -> Result<BundleWriter, String> {
        let file =
            File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        Ok(BundleWriter {
            zip: zip::ZipWriter::new(BufWriter::new(file)),
        })
    }

    /// Adds `bytes` as the entry `name` (`/`-separated).
    pub fn add(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        self.zip
            .start_file(name, options)
            .map_err(|e| format!("cannot add {name} to the bundle: {e}"))?;
        self.zip
            .write_all(bytes)
            .map_err(|e| format!("cannot add {name} to the bundle: {e}"))
    }

    /// Writes the zip's directory and closes the file.
    pub fn finish(self) -> Result<(), String> {
        let mut out = self
            .zip
            .finish()
            .map_err(|e| format!("cannot finish the bundle: {e}"))?;
        out.flush()
            .map_err(|e| format!("cannot finish the bundle: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem_geo_tiff_evaluator::{AffineTransform, CrsKey};

    /// A `w` x `h` north-up raster of 1 m pixels at (1000, 2000), value
    /// `f(col, row)`.
    fn raster(w: usize, h: usize, f: impl Fn(usize, usize) -> f64) -> GeoRaster {
        GeoRaster {
            width: w,
            height: h,
            values: (0..w * h).map(|i| f(i % w, i / w)).collect(),
            transform: AffineTransform {
                origin_x: 1000.0,
                origin_y: 2000.0,
                col_x: 1.0,
                row_x: 0.0,
                col_y: 0.0,
                row_y: -1.0,
            },
            crs: CrsKey::Projected(3006),
        }
    }

    #[test]
    fn a_grid_covers_its_raster_in_coarser_cells() {
        let grid = Grid::covering(&raster(10, 6, |_, _| 0.0), 2.0, 3006);
        assert_eq!((grid.width, grid.height), (5, 3));
        assert_eq!(grid.center(0, 0), (1001.0, 1999.0));
        assert_eq!(grid.cell_of(1009.9, 1994.1), Some((4, 2)));
        assert_eq!(grid.cell_of(1010.0, 1999.0), None);
    }

    #[test]
    fn block_mean_averages_and_skips_missing_pixels() {
        let r = raster(4, 2, |c, row| {
            if c == 0 && row == 0 {
                f64::NAN
            } else {
                c as f64
            }
        });
        let grid = Grid::covering(&r, 2.0, 3006);
        // Cell 0 holds columns 0, 1 with one missing (1, 0, 1); cell 1 holds 2, 3.
        assert_eq!(block_mean(&r, &grid), vec![2.0 / 3.0, 2.5]);
        let all_missing = raster(2, 2, |_, _| f64::NAN);
        assert!(block_mean(&all_missing, &Grid::covering(&all_missing, 2.0, 3006))[0].is_nan());
    }

    #[test]
    fn sampling_onto_a_grid_reads_its_cell_centers() {
        let r = raster(4, 4, |c, _| c as f64 + 0.5);
        let grid = Grid::covering(&r, 2.0, 3006);
        // Cell centers at x = 1001, 1003: halfway between pixel centers.
        assert_eq!(sample_onto(&r, &grid, 0.0), vec![1.0, 3.0, 1.0, 3.0]);
        assert_eq!(sample_onto(&r, &grid, 10.0)[0], 11.0);
    }

    #[test]
    fn a_cell_is_water_when_most_of_it_is() {
        let r = raster(4, 2, |_, _| 0.0);
        let mask = [true, true, true, false, true, false, false, false];
        let grid = Grid::covering(&r, 2.0, 3006);
        assert_eq!(mask_onto(&mask, &r, &grid), vec![1, 0]);
    }

    #[test]
    fn json_is_well_formed() {
        let json = Obj::new()
            .str("name", "a \"quoted\"\nline")
            .num("nan", f64::NAN)
            .num("whole", 5.0)
            .num("half", 2.5)
            .opt_int("missing", None)
            .raw("list", array(["1".to_string(), "2".to_string()]))
            .build();
        assert_eq!(
            json,
            r#"{"name":"a \"quoted\"\nline","nan":null,"whole":5,"half":2.5,"missing":null,"list":[1,2]}"#
        );
    }

    #[test]
    fn lines_carry_their_band_error() {
        let lines = vec![
            Line {
                points: vec![(0.0, 0.0), (3.0, 4.0)],
                level: 10.0,
            },
            Line {
                points: vec![(0.0, 0.0), (1.0, 0.0)],
                level: 15.0,
            },
        ];
        let levels = LevelAccuracy {
            band_offset: 1,
            predicted: vec![Some(15.2), None],
            ..LevelAccuracy::default()
        };
        assert_eq!(
            lines_json(&lines, &levels, 5.0, 0.0),
            r#"[{"level":10,"predicted":15.2,"error":0,"length_m":5,"points":[[0,0],[3,4]]},{"level":15,"predicted":null,"error":null,"length_m":1,"points":[[0,0],[1,0]]}]"#
        );
    }

    #[test]
    fn a_bundle_is_a_zip_with_its_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.zip");
        let mut writer = BundleWriter::create(&path).unwrap();
        writer.add("results.json", b"{}").unwrap();
        writer
            .add("sample_0001/truth.f32", &f32_bytes(&[1.5, f32::NAN]))
            .unwrap();
        writer.finish().unwrap();
        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut zip.by_name("sample_0001/truth.f32").unwrap(),
            &mut bytes,
        )
        .unwrap();
        assert_eq!(&bytes[..4], &1.5f32.to_le_bytes());
        assert!(f32::from_le_bytes(bytes[4..8].try_into().unwrap()).is_nan());
    }
}
