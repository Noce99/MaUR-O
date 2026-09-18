//! Scoring a predicted DEM against a ground-truth one: reading both as
//! georeferenced single-band TIFFs ([`read`]), restricting the comparison to
//! their shared ground and resampling the prediction onto the ground
//! truth's own grid ([`evaluate`]), and writing the result out as a report
//! and a couple of colormap PNGs ([`write_report`], [`write_abs_error_png`],
//! [`write_signed_error_png`], [`write_histogram_png`]) -- what the
//! `dem_geo_tiff_evaluator` binary is built on.
//!
//! Both files must already share the same CRS -- reprojecting one onto the
//! other is out of scope. Each file's own affine may carry a rotation (a
//! map's own grivation, in this crate's terms -- see [`crate::geotiff`]'s
//! doc comment; a real elevation TIFF from this project's own pipeline
//! almost always has one), and the two files' rotations don't even need to
//! match: [`evaluate`] samples the prediction at every ground-truth pixel's
//! own world position, through the prediction's own affine, whatever that
//! affine is, rather than intersecting two rectangles up front (which only
//! works when both are axis-aligned -- two *differently* rotated rectangles
//! intersect in a general polygon, not a rectangle).
//!
//! A DEM is only ever meaningful up to an additive constant (an arbitrary
//! vertical datum/offset -- the same fact `Contours-to-Raster.md`'s own
//! opening line makes about this crate's own elevation output), so
//! [`evaluate`] shifts the prediction by the one constant that minimizes its
//! mean squared error against the ground truth before scoring it at all.

use std::path::Path;

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::ColorType;

const MODEL_PIXEL_SCALE_TAG: u16 = 33550;
const MODEL_TIEPOINT_TAG: u16 = 33922;
const MODEL_TRANSFORMATION_TAG: u16 = 34264;
const GEO_KEY_DIRECTORY_TAG: u16 = 34735;
const GDAL_NODATA_TAG: u16 = 42113;

const PROJECTED_CS_TYPE_GEO_KEY: u16 = 3072;
const GEOGRAPHIC_TYPE_GEO_KEY: u16 = 2048;

/// A pixel-corner-to-world affine, in full generality (`GeoTIFF`'s own
/// `ModelTransformationTag` shape, minus the unused elevation row/column):
/// pixel `(col, row)`'s own upper-left corner sits at `(origin_x +
/// col*col_x + row*row_x, origin_y + col*col_y + row*row_y)`. `row_x`/
/// `col_y` are zero for an axis-aligned (north-up) raster and nonzero for a
/// rotated one -- a map's own grivation, in this crate's terms; see
/// [`crate::geotiff`]'s doc comment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineTransform {
    /// World X at pixel `(0, 0)`'s own upper-left corner.
    pub origin_x: f64,
    /// World Y at pixel `(0, 0)`'s own upper-left corner.
    pub origin_y: f64,
    /// World-X step per unit column.
    pub col_x: f64,
    /// World-X step per unit row -- zero unless the raster is rotated.
    pub row_x: f64,
    /// World-Y step per unit column -- zero unless the raster is rotated.
    pub col_y: f64,
    /// World-Y step per unit row.
    pub row_y: f64,
}

impl AffineTransform {
    /// World coordinates of pixel-space point `(col, row)` -- a fractional
    /// coordinate lands inside the pixel it rounds down to.
    pub fn pixel_to_world(&self, col: f64, row: f64) -> (f64, f64) {
        (
            self.origin_x + col * self.col_x + row * self.row_x,
            self.origin_y + col * self.col_y + row * self.row_y,
        )
    }

    /// The inverse of [`Self::pixel_to_world`], by solving the 2x2 linear
    /// system directly -- `(f64::NAN, f64::NAN)` if the transform is
    /// degenerate (zero determinant, which [`read`] already refuses to
    /// produce).
    pub fn world_to_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        let det = self.col_x * self.row_y - self.row_x * self.col_y;
        if det == 0.0 {
            return (f64::NAN, f64::NAN);
        }
        let dx = x - self.origin_x;
        let dy = y - self.origin_y;
        (
            (dx * self.row_y - dy * self.row_x) / det,
            (dy * self.col_x - dx * self.col_y) / det,
        )
    }
}

/// Which coordinate reference system a `GeoKeyDirectoryTag` names, read only
/// far enough to tell two files' own CRS apart -- an actual EPSG lookup or
/// reprojection between different ones is out of scope (see this module's
/// own doc comment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrsKey {
    /// A projected (linear-unit) CRS, by its EPSG code.
    Projected(u32),
    /// A geographic (lat/lon) CRS, by its EPSG code.
    Geographic(u32),
    /// No `ProjectedCSTypeGeoKey`/`GeographicTypeGeoKey` found at all.
    Unspecified,
}

/// `keys` is a raw `GeoKeyDirectoryTag`: a 4-short header
/// (`KeyDirectoryVersion, KeyRevision, MinorRevision, NumberOfKeys`)
/// followed by `NumberOfKeys` 4-short rows (`KeyID, TIFFTagLocation, Count,
/// Value_Offset`) -- the exact layout [`crate::geotiff::geo_key_directory`]
/// writes. Only a `SHORT` key stored directly in its own row (`
/// TIFFTagLocation == 0`) is read; a CS code is always written that way by
/// every encoder this tool has to deal with (this crate's own included), so
/// anything else is simply not a CS code this can see.
fn parse_geo_key_directory(keys: &[u16]) -> CrsKey {
    let Some(&num_keys) = keys.get(3) else {
        return CrsKey::Unspecified;
    };
    let mut projected = None;
    let mut geographic = None;
    for i in 0..num_keys as usize {
        let base = 4 + i * 4;
        let Some(row) = keys.get(base..base + 4) else {
            break;
        };
        let (key_id, location, value) = (row[0], row[1], row[3]);
        if location != 0 {
            continue;
        }
        match key_id {
            PROJECTED_CS_TYPE_GEO_KEY => projected = Some(value as u32),
            GEOGRAPHIC_TYPE_GEO_KEY => geographic = Some(value as u32),
            _ => {}
        }
    }
    match (projected, geographic) {
        (Some(p), _) => CrsKey::Projected(p),
        (None, Some(g)) => CrsKey::Geographic(g),
        (None, None) => CrsKey::Unspecified,
    }
}

/// A single-band raster read off a georeferenced TIFF: pixel values in
/// row-major order, `f64::NAN` wherever the source declared no data (either
/// its own `GDAL_NODATA` value or a genuine `NaN` sample), the
/// pixel-corner-to-world [`AffineTransform`] it sits at, and the CRS it
/// claims to be in.
#[derive(Clone, Debug)]
pub struct GeoRaster {
    /// Raster width, in pixels.
    pub width: usize,
    /// Raster height, in pixels.
    pub height: usize,
    /// Pixel values, row-major (`values[row * width + col]`); `f64::NAN`
    /// wherever the source had no data.
    pub values: Vec<f64>,
    /// The pixel-corner-to-world affine.
    pub transform: AffineTransform,
    /// The CRS the file claims, as far as [`CrsKey`] tells them apart.
    pub crs: CrsKey,
}

impl GeoRaster {
    /// The value at pixel `(col, row)`, or `None` where it is out of bounds
    /// or has no data.
    pub fn get(&self, col: usize, row: usize) -> Option<f64> {
        if col >= self.width || row >= self.height {
            return None;
        }
        let v = self.values[row * self.width + col];
        if v.is_nan() {
            None
        } else {
            Some(v)
        }
    }
}

fn decoding_result_to_f64(result: DecodingResult) -> Result<Vec<f64>, String> {
    Ok(match result {
        DecodingResult::U8(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::U16(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::U32(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::U64(v) => v.into_iter().map(|x| x as f64).collect(),
        DecodingResult::I8(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::I16(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::I32(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::I64(v) => v.into_iter().map(|x| x as f64).collect(),
        DecodingResult::F32(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::F64(v) => v,
        DecodingResult::F16(_) => return Err("16-bit float samples are not supported".to_string()),
    })
}

fn read_nodata(decoder: &mut Decoder<std::fs::File>) -> Result<Option<f64>, String> {
    match decoder.find_tag(Tag::Unknown(GDAL_NODATA_TAG)) {
        Ok(Some(value)) => {
            let text = value.into_string().map_err(|e| e.to_string())?;
            let text = text.trim();
            text.parse::<f64>()
                .map(Some)
                .map_err(|e| format!("bad GDAL_NODATA value {text:?}: {e}"))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads the raster's own pixel-corner-to-world transform: from a
/// `ModelTransformationTag` if it has one (what
/// [`crate::geotiff::model_transformation`] writes -- rotation and all),
/// else from a `ModelPixelScaleTag`+`ModelTiepointTag` pair (what
/// real-world DEM exports -- SRTM, LiDAR-derived rasters, GIS software in
/// general -- almost always carry instead, assuming a single tiepoint;
/// always axis-aligned, since that pair can't express a rotation at all).
fn read_transform(decoder: &mut Decoder<std::fs::File>) -> Result<AffineTransform, String> {
    if let Some(value) = decoder
        .find_tag(Tag::Unknown(MODEL_TRANSFORMATION_TAG))
        .map_err(|e| e.to_string())?
    {
        let m = value.into_f64_vec().map_err(|e| e.to_string())?;
        if m.len() != 16 {
            return Err(format!(
                "ModelTransformationTag has {} value(s), expected 16",
                m.len()
            ));
        }
        let transform = AffineTransform {
            origin_x: m[3],
            origin_y: m[7],
            col_x: m[0],
            row_x: m[1],
            col_y: m[4],
            row_y: m[5],
        };
        let det = transform.col_x * transform.row_y - transform.row_x * transform.col_y;
        if det == 0.0 {
            return Err("ModelTransformationTag is degenerate (zero determinant)".to_string());
        }
        return Ok(transform);
    }

    let scale = decoder
        .find_tag(Tag::Unknown(MODEL_PIXEL_SCALE_TAG))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "not georeferenced: no ModelTransformationTag or ModelPixelScaleTag".to_string()
        })?
        .into_f64_vec()
        .map_err(|e| e.to_string())?;
    let tie = decoder
        .find_tag(Tag::Unknown(MODEL_TIEPOINT_TAG))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "not georeferenced: has a ModelPixelScaleTag but no ModelTiepointTag".to_string()
        })?
        .into_f64_vec()
        .map_err(|e| e.to_string())?;
    if scale.len() < 2 || tie.len() < 5 {
        return Err(
            "ModelPixelScaleTag/ModelTiepointTag too short to be real georeferencing"
                .to_string(),
        );
    }
    if scale[0] == 0.0 || scale[1] == 0.0 {
        return Err("ModelPixelScaleTag has a zero pixel size".to_string());
    }
    // A single tiepoint (raster I, J -> model X, Y): model(col, row) =
    // (X - I*scaleX + col*scaleX, Y + J*scaleY - row*scaleY).
    Ok(AffineTransform {
        origin_x: tie[3] - tie[0] * scale[0],
        origin_y: tie[4] + tie[1] * scale[1],
        col_x: scale[0],
        row_x: 0.0,
        col_y: 0.0,
        row_y: -scale[1],
    })
}

fn read_crs(decoder: &mut Decoder<std::fs::File>) -> Result<CrsKey, String> {
    match decoder.find_tag(Tag::Unknown(GEO_KEY_DIRECTORY_TAG)) {
        Ok(Some(value)) => {
            let keys = value.into_u16_vec().map_err(|e| e.to_string())?;
            Ok(parse_geo_key_directory(&keys))
        }
        Ok(None) => Ok(CrsKey::Unspecified),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads `path` as a single-band georeferenced DEM: its pixel values, its
/// pixel-corner-to-world transform (rotation and all -- see
/// [`AffineTransform`]), and the CRS it claims to be in.
///
/// Rejected as an error if the file isn't single-band, isn't georeferenced
/// at all, or its own transform is degenerate (a zero determinant, which no
/// real raster should ever produce).
pub fn read(path: &Path) -> Result<GeoRaster, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut decoder =
        Decoder::new(file).map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    let (width, height) = decoder
        .dimensions()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (width, height) = (width as usize, height as usize);

    match decoder.colortype() {
        Ok(ColorType::Gray(_)) => {}
        Ok(other) => {
            return Err(format!(
                "{}: expected a single-band DEM, found colortype {other:?}",
                path.display()
            ))
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    }

    let nodata = read_nodata(&mut decoder).map_err(|e| format!("{}: {e}", path.display()))?;
    let transform = read_transform(&mut decoder).map_err(|e| format!("{}: {e}", path.display()))?;
    let crs = read_crs(&mut decoder).map_err(|e| format!("{}: {e}", path.display()))?;

    let image = decoder
        .read_image()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut values =
        decoding_result_to_f64(image).map_err(|e| format!("{}: {e}", path.display()))?;
    if values.len() != width * height {
        return Err(format!(
            "{}: expected {} pixel(s), decoded {}",
            path.display(),
            width * height,
            values.len()
        ));
    }
    if let Some(nodata) = nodata {
        for v in &mut values {
            if *v == nodata {
                *v = f64::NAN;
            }
        }
    }

    Ok(GeoRaster {
        width,
        height,
        values,
        transform,
        crs,
    })
}

/// Bilinearly samples `raster` at world coordinates `(x, y)`, or `None` if
/// the nearest in-bounds pixel in any direction has no data.
/// `world_to_pixel` gives a pixel-*corner* coordinate; this shifts back half
/// a pixel first, since a sample value belongs to its pixel's own center,
/// not its corner.
///
/// `None` if `(x, y)` falls outside `raster`'s own declared coverage at
/// all -- this is also what tells [`evaluate`] a ground-truth pixel has no
/// counterpart in the prediction, replacing an explicit intersection
/// computed up front. A neighbor index that lands one past the raster's own
/// edge but *inside* that coverage -- which happens for any query point in
/// its own outer half-pixel, including every point exactly on a shared grid
/// line between two same-resolution rasters -- is clamped to the edge
/// rather than treated as missing: its interpolation weight there is small
/// (zero exactly on the grid line), and only ever reaches the edge's own
/// value, never truly extrapolating past it.
fn bilinear_sample(raster: &GeoRaster, x: f64, y: f64) -> Option<f64> {
    if raster.width == 0 || raster.height == 0 {
        return None;
    }
    let (px, py) = raster.transform.world_to_pixel(x, y);
    let col = px - 0.5;
    let row = py - 0.5;
    if !col.is_finite() || !row.is_finite() {
        return None;
    }
    // A small tolerance against floating-point noise right on the boundary
    // -- not a license to extrapolate meaningfully past it.
    const EPS: f64 = 1e-6;
    if col < -0.5 - EPS
        || row < -0.5 - EPS
        || col > raster.width as f64 - 0.5 + EPS
        || row > raster.height as f64 - 0.5 + EPS
    {
        return None;
    }
    let col0 = col.floor();
    let row0 = row.floor();
    let fx = col - col0;
    let fy = row - row0;

    let clamp_col = |c: f64| c.clamp(0.0, (raster.width - 1) as f64) as usize;
    let clamp_row = |r: f64| r.clamp(0.0, (raster.height - 1) as f64) as usize;

    let v00 = raster.get(clamp_col(col0), clamp_row(row0))?;
    let v10 = raster.get(clamp_col(col0 + 1.0), clamp_row(row0))?;
    let v01 = raster.get(clamp_col(col0), clamp_row(row0 + 1.0))?;
    let v11 = raster.get(clamp_col(col0 + 1.0), clamp_row(row0 + 1.0))?;

    let top = v00 * (1.0 - fx) + v10 * fx;
    let bottom = v01 * (1.0 - fx) + v11 * fx;
    Some(top * (1.0 - fy) + bottom * fy)
}

/// One evaluation run's own numbers and per-pixel error grid, at the
/// ground truth's own full resolution and extent, with the prediction
/// resampled onto it and corrected by [`Self::best_constant`]. A pixel the
/// prediction does not cover at all is `f64::NAN` in [`Self::signed_error`]
/// and excluded from every number below.
#[derive(Clone, Debug)]
pub struct Evaluation {
    /// Output grid width, in pixels -- the ground truth's own width.
    pub width: usize,
    /// Output grid height, in pixels.
    pub height: usize,
    /// The additive constant added to the resampled prediction before
    /// scoring -- the exact minimizer of [`Self::mse`] (see [`evaluate`]'s
    /// own doc comment).
    pub best_constant: f64,
    /// `(prediction + best_constant) - ground_truth` at every pixel with a
    /// value on both sides, row-major; `f64::NAN` elsewhere.
    pub signed_error: Vec<f64>,
    /// How many of `width * height` pixels have a value on both sides.
    pub valid_pixel_count: usize,
    /// Mean squared error over the valid pixels.
    pub mse: f64,
    /// `mse.sqrt()`.
    pub rmse: f64,
    /// Mean absolute error over the valid pixels.
    pub mae: f64,
    /// The largest absolute error over the valid pixels; `0.0` if there is
    /// exactly one and it is a perfect match.
    pub max_abs_error: f64,
}

/// Scores `prediction` against `ground_truth` over their shared ground, at
/// `ground_truth`'s own resolution and extent: every ground-truth pixel's
/// own world position is looked up in `prediction`, bilinearly resampled
/// through `prediction`'s own transform -- whatever that transform is, so
/// the two files' pixel sizes, origins and rotations may all differ freely
/// -- and left `f64::NAN` where `prediction` does not cover it at all (see
/// [`bilinear_sample`]'s own doc comment). There is deliberately no upfront
/// intersection-of-two-rectangles step: that only works when both rasters
/// are axis-aligned, and two *differently* rotated rectangles intersect in
/// a general polygon, not a rectangle.
///
/// Since a DEM is only ever meaningful up to an additive constant,
/// `prediction` is shifted by the one constant minimizing the mean squared
/// error against `ground_truth` before either is scored. MSE is quadratic
/// (and convex) in that constant, so its minimizer has a closed form --
/// `mean(ground_truth - prediction)` over the pixels both cover -- rather
/// than needing an actual search.
///
/// Both rasters must already share the same CRS (see [`CrsKey`]); this never
/// reprojects one onto the other.
pub fn evaluate(ground_truth: &GeoRaster, prediction: &GeoRaster) -> Result<Evaluation, String> {
    if ground_truth.crs == CrsKey::Unspecified {
        return Err("the ground truth file has no recognizable CRS".to_string());
    }
    if prediction.crs == CrsKey::Unspecified {
        return Err("the prediction file has no recognizable CRS".to_string());
    }
    if ground_truth.crs != prediction.crs {
        return Err(format!(
            "the two files are not in the same CRS ({:?} vs {:?})",
            ground_truth.crs, prediction.crs
        ));
    }

    let width = ground_truth.width;
    let height = ground_truth.height;

    let mut gt_values = vec![f64::NAN; width * height];
    let mut pred_values = vec![f64::NAN; width * height];
    for row in 0..height {
        for col in 0..width {
            let idx = row * width + col;
            gt_values[idx] = ground_truth.get(col, row).unwrap_or(f64::NAN);

            let (wx, wy) = ground_truth
                .transform
                .pixel_to_world(col as f64 + 0.5, row as f64 + 0.5);
            pred_values[idx] = bilinear_sample(prediction, wx, wy).unwrap_or(f64::NAN);
        }
    }

    let mut sum_diff = 0.0;
    let mut n = 0usize;
    for (gt, pred) in gt_values.iter().zip(&pred_values) {
        if !gt.is_nan() && !pred.is_nan() {
            sum_diff += gt - pred;
            n += 1;
        }
    }
    if n == 0 {
        return Err("the two rasters share no pixel with a value on both sides".to_string());
    }
    let best_constant = sum_diff / n as f64;

    let mut signed_error = vec![f64::NAN; width * height];
    let mut sum_sq = 0.0;
    let mut sum_abs = 0.0;
    let mut max_abs_error = 0.0f64;
    for i in 0..width * height {
        if gt_values[i].is_nan() || pred_values[i].is_nan() {
            continue;
        }
        let err = (pred_values[i] + best_constant) - gt_values[i];
        signed_error[i] = err;
        sum_sq += err * err;
        sum_abs += err.abs();
        max_abs_error = max_abs_error.max(err.abs());
    }
    let mse = sum_sq / n as f64;
    let mae = sum_abs / n as f64;

    Ok(Evaluation {
        width,
        height,
        best_constant,
        signed_error,
        valid_pixel_count: n,
        mse,
        rmse: mse.sqrt(),
        mae,
        max_abs_error,
    })
}

/// Writes `evaluation`'s own numbers as a plain-text report; `gt_path` and
/// `prediction_path` are recorded for provenance, since the numbers alone
/// don't say what was compared.
pub fn write_report(
    evaluation: &Evaluation,
    gt_path: &Path,
    prediction_path: &Path,
    path: &Path,
) -> Result<(), String> {
    let text = format!(
        "ground truth: {}\n\
         prediction: {}\n\
         \n\
         output grid: {} x {} px (the ground truth's own full grid)\n\
         valid pixel(s) scored: {}\n\
         \n\
         best additive constant: {:.6}\n\
         MSE: {:.6}\n\
         RMSE: {:.6}\n\
         MAE: {:.6}\n\
         max absolute error: {:.6}\n",
        gt_path.display(),
        prediction_path.display(),
        evaluation.width,
        evaluation.height,
        evaluation.valid_pixel_count,
        evaluation.best_constant,
        evaluation.mse,
        evaluation.rmse,
        evaluation.mae,
        evaluation.max_abs_error,
    );
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A gray distinct from either colormap below, for a pixel with no value on
/// both sides.
const NO_DATA_COLOR: [u8; 3] = [0x80, 0x80, 0x80];

/// A perceptually-uniform sequential colormap (viridis's own control
/// points): dark purple at `t = 0`, yellow at `t = 1`. Used for
/// [`write_abs_error_png`].
const VIRIDIS_STOPS: [(f64, [u8; 3]); 5] = [
    (0.0, [0x44, 0x01, 0x54]),
    (0.25, [0x3b, 0x52, 0x8b]),
    (0.5, [0x21, 0x90, 0x8c]),
    (0.75, [0x5d, 0xc9, 0x63]),
    (1.0, [0xfd, 0xe7, 0x25]),
];

/// A diverging colormap (coolwarm's own endpoints): blue below zero, white
/// at zero, red above -- so a color's own hue says which side of the ground
/// truth a pixel landed on. Used for [`write_signed_error_png`].
const DIVERGING_STOPS: [(f64, [u8; 3]); 3] = [
    (-1.0, [0x3b, 0x4c, 0xc0]),
    (0.0, [0xff, 0xff, 0xff]),
    (1.0, [0xb4, 0x04, 0x26]),
];

/// [`stops`] linearly interpolated at `t` (clamped to the stops' own
/// range).
fn lerp_stops(stops: &[(f64, [u8; 3])], t: f64) -> [u8; 3] {
    let t = t.clamp(stops[0].0, stops[stops.len() - 1].0);
    for pair in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return std::array::from_fn(|i| lerp_u8(c0[i], c1[i], f));
        }
    }
    stops[stops.len() - 1].1
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

fn write_rgb_png(pixels: &[u8], width: usize, height: usize, path: &Path) -> Result<(), String> {
    let file =
        std::fs::File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    writer
        .write_image_data(pixels)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    writer
        .finish()
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Writes `evaluation.signed_error`'s own absolute value as a sequential
/// colormap PNG, scaled from `0` to this run's own
/// [`Evaluation::max_abs_error`] -- gray wherever a pixel has no value on
/// both sides.
pub fn write_abs_error_png(evaluation: &Evaluation, path: &Path) -> Result<(), String> {
    let scale = if evaluation.max_abs_error > 0.0 {
        evaluation.max_abs_error
    } else {
        1.0
    };
    let mut pixels = Vec::with_capacity(evaluation.width * evaluation.height * 3);
    for &err in &evaluation.signed_error {
        let rgb = if err.is_nan() {
            NO_DATA_COLOR
        } else {
            lerp_stops(&VIRIDIS_STOPS, err.abs() / scale)
        };
        pixels.extend_from_slice(&rgb);
    }
    write_rgb_png(&pixels, evaluation.width, evaluation.height, path)
}

/// Writes [`Evaluation::signed_error`] as a diverging colormap PNG, scaled
/// symmetrically around zero by this run's own
/// [`Evaluation::max_abs_error`] -- gray wherever a pixel has no value on
/// both sides.
pub fn write_signed_error_png(evaluation: &Evaluation, path: &Path) -> Result<(), String> {
    let scale = if evaluation.max_abs_error > 0.0 {
        evaluation.max_abs_error
    } else {
        1.0
    };
    let mut pixels = Vec::with_capacity(evaluation.width * evaluation.height * 3);
    for &err in &evaluation.signed_error {
        let rgb = if err.is_nan() {
            NO_DATA_COLOR
        } else {
            lerp_stops(&DIVERGING_STOPS, err / scale)
        };
        pixels.extend_from_slice(&rgb);
    }
    write_rgb_png(&pixels, evaluation.width, evaluation.height, path)
}

const HISTOGRAM_WIDTH: usize = 640;
const HISTOGRAM_HEIGHT: usize = 320;
const HISTOGRAM_BINS: usize = 40;
const HISTOGRAM_MARGIN: usize = 20;

fn set_pixel(pixels: &mut [u8], width: usize, x: usize, y: usize, rgb: [u8; 3]) {
    let idx = (y * width + x) * 3;
    pixels[idx..idx + 3].copy_from_slice(&rgb);
}

/// Writes a histogram PNG of [`Evaluation::signed_error`] over its valid
/// pixels, binned across `[-max_abs_error, max_abs_error]` so it lines up
/// with [`write_signed_error_png`]'s own scale; a red vertical line marks
/// zero error.
pub fn write_histogram_png(evaluation: &Evaluation, path: &Path) -> Result<(), String> {
    let scale = if evaluation.max_abs_error > 0.0 {
        evaluation.max_abs_error
    } else {
        1.0
    };
    let mut counts = vec![0u32; HISTOGRAM_BINS];
    for &err in &evaluation.signed_error {
        if err.is_nan() {
            continue;
        }
        let t = (err / scale + 1.0) / 2.0;
        let bin = ((t * HISTOGRAM_BINS as f64) as isize).clamp(0, HISTOGRAM_BINS as isize - 1);
        counts[bin as usize] += 1;
    }
    let max_count = counts.iter().copied().max().unwrap_or(0).max(1);

    let mut pixels = vec![0xffu8; HISTOGRAM_WIDTH * HISTOGRAM_HEIGHT * 3];
    let plot_left = HISTOGRAM_MARGIN;
    let plot_right = HISTOGRAM_WIDTH - HISTOGRAM_MARGIN;
    let plot_bottom = HISTOGRAM_HEIGHT - HISTOGRAM_MARGIN;
    let plot_top = HISTOGRAM_MARGIN;
    let plot_width = plot_right - plot_left;
    let plot_height = plot_bottom - plot_top;

    let bar_width = plot_width as f64 / HISTOGRAM_BINS as f64;
    for (i, &count) in counts.iter().enumerate() {
        let bar_height = (count as f64 / max_count as f64 * plot_height as f64).round() as usize;
        let x0 = plot_left + (i as f64 * bar_width).round() as usize;
        let x1 = (plot_left + ((i + 1) as f64 * bar_width).round() as usize).min(plot_right);
        for y in (plot_bottom - bar_height)..plot_bottom {
            for x in x0..x1 {
                set_pixel(&mut pixels, HISTOGRAM_WIDTH, x, y, [0x21, 0x90, 0x8c]);
            }
        }
    }

    for x in plot_left..plot_right {
        set_pixel(&mut pixels, HISTOGRAM_WIDTH, x, plot_bottom - 1, [0, 0, 0]);
    }
    let zero_x = (plot_left + (0.5 * plot_width as f64).round() as usize).min(plot_right - 1);
    for y in plot_top..plot_bottom {
        set_pixel(&mut pixels, HISTOGRAM_WIDTH, zero_x, y, [0xb4, 0x04, 0x26]);
    }

    write_rgb_png(&pixels, HISTOGRAM_WIDTH, HISTOGRAM_HEIGHT, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::encoder::colortype::Gray32Float;
    use tiff::encoder::TiffEncoder;

    fn transform(origin_x: f64, origin_y: f64, px_size: f64) -> AffineTransform {
        AffineTransform {
            origin_x,
            origin_y,
            col_x: px_size,
            row_x: 0.0,
            col_y: 0.0,
            row_y: -px_size,
        }
    }

    fn rotated_transform(origin_x: f64, origin_y: f64, px_size: f64, degrees: f64) -> AffineTransform {
        let (sin, cos) = degrees.to_radians().sin_cos();
        AffineTransform {
            origin_x,
            origin_y,
            col_x: px_size * cos,
            row_x: px_size * sin,
            col_y: px_size * sin,
            row_y: -px_size * cos,
        }
    }

    fn raster(
        width: usize,
        height: usize,
        values: Vec<f64>,
        origin_x: f64,
        origin_y: f64,
        px_size: f64,
        epsg: u32,
    ) -> GeoRaster {
        GeoRaster {
            width,
            height,
            values,
            transform: transform(origin_x, origin_y, px_size),
            crs: CrsKey::Projected(epsg),
        }
    }

    fn rotated_raster(
        width: usize,
        height: usize,
        values: Vec<f64>,
        origin_x: f64,
        origin_y: f64,
        px_size: f64,
        degrees: f64,
        epsg: u32,
    ) -> GeoRaster {
        GeoRaster {
            width,
            height,
            values,
            transform: rotated_transform(origin_x, origin_y, px_size, degrees),
            crs: CrsKey::Projected(epsg),
        }
    }

    #[test]
    fn pixel_to_world_and_back_round_trips() {
        let t = transform(100.0, 200.0, 2.0);
        let (x, y) = t.pixel_to_world(3.0, 4.0);
        let (col, row) = t.world_to_pixel(x, y);
        assert!((col - 3.0).abs() < 1e-9 && (row - 4.0).abs() < 1e-9);
    }

    #[test]
    fn parse_geo_key_directory_reads_a_projected_cs() {
        let keys = crate::geotiff::geo_key_directory(3006);
        assert_eq!(parse_geo_key_directory(&keys), CrsKey::Projected(3006));
    }

    #[test]
    fn parse_geo_key_directory_with_no_keys_is_unspecified() {
        assert_eq!(parse_geo_key_directory(&[]), CrsKey::Unspecified);
        assert_eq!(parse_geo_key_directory(&[1, 1, 0, 0]), CrsKey::Unspecified);
    }

    fn write_tiepoint_tiff(path: &Path, width: u32, height: u32, values: &[f32], origin: (f64, f64), px_size: f64, epsg: u16) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        let mut image = encoder.new_image::<Gray32Float>(width, height).unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(MODEL_PIXEL_SCALE_TAG), &[px_size, px_size, 0.0][..])
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::Unknown(MODEL_TIEPOINT_TAG),
                &[0.0, 0.0, 0.0, origin.0, origin.1, 0.0][..],
            )
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
                &crate::geotiff::geo_key_directory(epsg)[..],
            )
            .unwrap();
        image.write_data(values).unwrap();
    }

    #[test]
    fn reads_a_tiepoint_style_geotiff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gt.tif");
        write_tiepoint_tiff(&path, 2, 2, &[1.0, 2.0, 3.0, 4.0], (1000.0, 2000.0), 5.0, 3006);

        let g = read(&path).unwrap();
        assert_eq!((g.width, g.height), (2, 2));
        assert_eq!(g.values, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(g.crs, CrsKey::Projected(3006));
        assert_eq!(
            g.transform,
            AffineTransform {
                origin_x: 1000.0,
                origin_y: 2000.0,
                col_x: 5.0,
                row_x: 0.0,
                col_y: 0.0,
                row_y: -5.0,
            }
        );
    }

    #[test]
    fn reads_a_model_transformation_style_geotiff() {
        use crate::geotiff;
        use crate::map::Georeferencing;

        let georef = Georeferencing {
            epsg: 3006,
            grivation: 0.0,
            ..Georeferencing::default()
        };
        let origin = geo::Coord { x: 10.0, y: 20.0 };
        let px_size = 2.0;
        let transform_matrix = geotiff::model_transformation(&georef, origin, px_size).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pred.tif");
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        let mut image = encoder.new_image::<Gray32Float>(2, 2).unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(MODEL_TRANSFORMATION_TAG), &transform_matrix[..])
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
                &geotiff::geo_key_directory(3006)[..],
            )
            .unwrap();
        image.write_data(&[10.0f32, 20.0, 30.0, 40.0]).unwrap();

        let g = read(&path).unwrap();
        assert_eq!(g.crs, CrsKey::Projected(3006));
        // The raster's own (0, 0) pixel corner is `origin` placed on the
        // earth, not `origin` itself -- `ground_to_projected` flips the y
        // axis going from ground-meter to projected coordinates (see
        // `geotiff`'s own doc comment).
        let want = geotiff::ground_to_projected(&georef, origin).unwrap();
        let (x, y) = g.transform.pixel_to_world(0.0, 0.0);
        assert!((x - want.x).abs() < 1e-6 && (y - want.y).abs() < 1e-6);
    }

    #[test]
    fn reads_a_rotated_model_transformation_style_geotiff() {
        // A real map's own grivation (see crate::geotiff) turns into exactly
        // this kind of rotated ModelTransformationTag once exported -- this
        // must be read faithfully, not rejected.
        use crate::geotiff;
        use crate::map::Georeferencing;

        let georef = Georeferencing {
            epsg: 3006,
            grivation: 15.0,
            grivation_specified: true,
            ..Georeferencing::default()
        };
        let origin = geo::Coord { x: 0.0, y: 0.0 };
        let transform_matrix = geotiff::model_transformation(&georef, origin, 1.0).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotated.tif");
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        let mut image = encoder.new_image::<Gray32Float>(2, 2).unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(MODEL_TRANSFORMATION_TAG), &transform_matrix[..])
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
                &geotiff::geo_key_directory(3006)[..],
            )
            .unwrap();
        image.write_data(&[1.0f32, 2.0, 3.0, 4.0]).unwrap();

        let g = read(&path).unwrap();
        assert!(g.transform.row_x.abs() > 0.1, "expected a real rotation term");
        // Round-trips through the raster's own (now rotated) transform.
        let (x, y) = g.transform.pixel_to_world(0.0, 0.0);
        let (col, row) = g.transform.world_to_pixel(x, y);
        assert!((col - 0.0).abs() < 1e-9 && (row - 0.0).abs() < 1e-9);
    }

    #[test]
    fn nodata_pixels_become_nan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nodata.tif");
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        let mut image = encoder.new_image::<Gray32Float>(2, 1).unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(MODEL_PIXEL_SCALE_TAG), &[1.0, 1.0, 0.0][..])
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(MODEL_TIEPOINT_TAG), &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0][..])
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
                &crate::geotiff::geo_key_directory(3006)[..],
            )
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(GDAL_NODATA_TAG), "-9999")
            .unwrap();
        image.write_data(&[-9999.0f32, 5.0]).unwrap();

        let g = read(&path).unwrap();
        assert!(g.get(0, 0).is_none());
        assert_eq!(g.get(1, 0), Some(5.0));
    }

    #[test]
    fn evaluate_finds_the_exact_bias_between_two_identical_grids() {
        let gt = raster(2, 2, vec![1.0, 2.0, 3.0, 4.0], 0.0, 0.0, 1.0, 3006);
        let pred = raster(2, 2, vec![-4.0, -3.0, -2.0, -1.0], 0.0, 0.0, 1.0, 3006);
        let result = evaluate(&gt, &pred).unwrap();
        assert!((result.best_constant - 5.0).abs() < 1e-9);
        assert!(result.mse < 1e-9);
        assert_eq!(result.valid_pixel_count, 4);
    }

    #[test]
    fn evaluate_resamples_a_flat_prediction_at_a_different_resolution() {
        // A constant field survives bilinear resampling at any resolution,
        // so this exercises the resampling path without needing to hand
        // work out an interpolated value.
        let gt = raster(6, 6, vec![50.0; 36], 0.0, 0.0, 1.0, 3006);
        let pred = raster(6, 6, vec![45.0; 36], -2.0, 2.0, 2.0, 3006);
        let result = evaluate(&gt, &pred).unwrap();
        assert!((result.best_constant - 5.0).abs() < 1e-9, "{}", result.best_constant);
        assert!(result.mse < 1e-9);
        assert_eq!(result.valid_pixel_count, 36);
    }

    #[test]
    fn evaluate_rejects_mismatched_crs() {
        let gt = raster(2, 2, vec![1.0; 4], 0.0, 0.0, 1.0, 3006);
        let pred = raster(2, 2, vec![1.0; 4], 0.0, 0.0, 1.0, 4326);
        let err = evaluate(&gt, &pred).unwrap_err();
        assert!(err.contains("CRS"), "unexpected error: {err}");
    }

    #[test]
    fn evaluate_rejects_disjoint_rasters() {
        let gt = raster(2, 2, vec![1.0; 4], 0.0, 0.0, 1.0, 3006);
        let pred = raster(2, 2, vec![1.0; 4], 1000.0, 1000.0, 1.0, 3006);
        let err = evaluate(&gt, &pred).unwrap_err();
        assert!(err.contains("no pixel"), "unexpected error: {err}");
    }

    #[test]
    fn evaluate_handles_two_differently_rotated_rasters() {
        // Mirrors the real case that first exposed the need for this: a
        // ground truth DEM and this project's own elevation output, each
        // carrying its own real (and not quite identical) rotation. A
        // constant field still survives resampling exactly, whatever each
        // raster's own rotation is.
        let gt = rotated_raster(4, 4, vec![50.0; 16], 0.0, 0.0, 1.0, 7.1, 3006);
        let pred = rotated_raster(4, 4, vec![45.0; 16], 0.0, 0.0, 1.0, 7.100001, 3006);
        let result = evaluate(&gt, &pred).unwrap();
        assert!((result.best_constant - 5.0).abs() < 1e-6, "{}", result.best_constant);
        assert!(result.mse < 1e-9);
        assert!(result.valid_pixel_count > 0);
    }
}
