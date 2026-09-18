//! Placing a raster on the earth: turning a [`Georeferencing`] (what
//! `Contours-to-Raster.md`'s own elevation TIFF has never carried, see
//! `step5_elevation_raster::write_tiff`'s doc comment) into the affine
//! pixel-to-projected-coordinate transform and the GeoTIFF tags a DEM needs
//! to open at the right place in a GIS tool.
//!
//! [`ground_to_projected`] reproduces, exactly, what OpenOrienteering
//! Mapper's own `Georeferencing::updateTransformation()` computes (source:
//! `src/core/georeferencing.cpp`) -- a rotation by `-grivation` composed
//! with a ground-meter scale and a north-is-up flip (`ContourRaster`'s own
//! ground meters keep the map's paper convention of `y` growing *downward*,
//! same as `Point`'s own doc comment already says), then a translation to
//! the projected reference point. It assumes the file's own `map_ref_point`
//! is `(0, 0)` -- the only value this crate's `xml_reader` ever reads one
//! as, since it does not parse a top-level `<georeferencing><ref_point>`
//! distinct from `<projected_crs><ref_point>` -- which is also exactly what
//! makes `ContourRaster`'s own ground-meter coordinates (`paper_mm *
//! meters_per_mm`, no offset: see `step1_extract::extract`) already sit in
//! Mapper's own "map coordinate" frame with nothing left to shift.
//!
//! [`model_transformation`] builds the full pixel-corner-to-projected affine
//! [`ground_to_projected`] alone can't express (a raster pixel, not a bare
//! ground-meter point, is what a GeoTIFF's own `ModelTransformationTag`
//! needs) by evaluating it at three points rather than hand-expanding the
//! algebra -- since the map is exactly affine, three points pin it down
//! exactly, and there is no arithmetic left here to get subtly wrong.
//!
//! [`geo_key_directory`] is the handful of GeoKeys a projected DEM actually
//! needs (`GTModelTypeGeoKey`, `GTRasterTypeGeoKeyRasterPixelIsArea`,
//! `ProjectedCSTypeGeoKey`, `ProjLinearUnitsGeoKey`), laid out by hand in the
//! exact binary shape the GeoTIFF 1.0 spec's own `GeoKeyDirectoryTag`
//! requires -- the `tiff` crate (0.11) has no GeoTIFF support of its own,
//! but its `Tag::Unknown` variant lets any crate write one raw.
//!
//! [`resample_north_up`] (with its own inverse, [`projected_to_ground`]) is
//! what actually gets *written* to a `.tif` on disk today, not
//! [`model_transformation`]: a rotated `ModelTransformationTag` is a
//! perfectly legal way to describe a grivated raster, but nothing
//! downstream this crate's own DEMs get fed into -- o-mnia's own custom
//! GeoTIFF importer among them -- reads a `ModelTransformationTag`'s shear
//! terms at all; they read `ModelPixelScaleTag`/`ModelTiepointTag` (plain
//! axis-aligned) and silently drop the rotation from a `ModelTransformation`
//! file instead of erroring, so a grivated map's own DEM came back
//! north-up-but-still-off-by-the-grivation-angle every time it was
//! re-opened. [`resample_north_up`] resamples the raster itself onto a
//! plain axis-aligned grid before it's ever written, so there is no
//! rotation left in the file for a reader to drop. [`model_transformation`]
//! is kept (and still tested) for `dem_geo_tiff_evaluator`, which *does*
//! read `ModelTransformationTag` correctly and uses it as a reference
//! fixture.

use geo::Coord;

use crate::map::Georeferencing;

/// `local` (a `ContourRaster`-style ground-meter coordinate, in the map's
/// own unrotated, paper-`y`-down frame) placed on the earth, in
/// `georef`'s own projected CRS -- `None` where `georef.epsg == 0` (the file
/// names no real CRS to place it in at all).
///
/// See this module's own doc comment for exactly which upstream formula this
/// ports and which of its own assumptions (a `(0, 0)` `map_ref_point`) let
/// `local` be used directly, with no further offset.
pub fn ground_to_projected(georef: &Georeferencing, local: Coord<f64>) -> Option<Coord<f64>> {
    if georef.epsg == 0 {
        return None;
    }
    let grivation = georef.grivation.to_radians();
    let (sin_g, cos_g) = grivation.sin_cos();
    let s = georef.auxiliary_scale_factor;
    Some(Coord {
        x: s * (local.x * cos_g - local.y * sin_g) + georef.ref_point_x,
        y: -s * (local.x * sin_g + local.y * cos_g) + georef.ref_point_y,
    })
}

/// Inverse of [`ground_to_projected`]: a point in `georef`'s own projected
/// CRS placed back onto the map's own unrotated, paper-`y`-down ground-meter
/// frame. `None` where `georef.epsg == 0`, matching `ground_to_projected`.
///
/// `ground_to_projected`'s own `(dx, dy) = R * (local.x, local.y)` (after
/// dividing out the scale `s`) uses `R = [[cosG, -sinG], [sinG, cosG]]`, a
/// proper rotation (determinant `1`), so `Rᵀ = R⁻¹` -- i.e. the same cosine/
/// sine pair, transposed, undoes it exactly rather than needing a general
/// matrix inverse.
pub fn projected_to_ground(georef: &Georeferencing, projected: Coord<f64>) -> Option<Coord<f64>> {
    if georef.epsg == 0 {
        return None;
    }
    let grivation = georef.grivation.to_radians();
    let (sin_g, cos_g) = grivation.sin_cos();
    let s = georef.auxiliary_scale_factor;
    let dx = (projected.x - georef.ref_point_x) / s;
    let dy = (georef.ref_point_y - projected.y) / s;
    Some(Coord {
        x: cos_g * dx + sin_g * dy,
        y: -sin_g * dx + cos_g * dy,
    })
}

/// A plain axis-aligned (north-up) raster in `georef`'s own projected CRS:
/// pixel `(0, 0)`'s own corner sits at `origin`, one pixel is `px_size` real
/// projected-CRS meters square (row 0 = the raster's own north edge), `data`
/// is `width * height` row-major, `f32::NAN` wherever [`resample_north_up`]'s
/// own `sample` closure had nothing to give.
pub struct NorthUpRaster {
    /// Grid width, in pixels.
    pub width: usize,
    /// Grid height, in pixels.
    pub height: usize,
    /// Pixel `(0, 0)`'s own corner, in the projected CRS.
    pub origin: Coord<f64>,
    /// Pixel size, in real projected-CRS meters (both axes, square pixels).
    pub px_size: f64,
    /// Row-major sample values, `f32::NAN` wherever `sample` had nothing.
    pub data: Vec<f32>,
}

/// Resamples a raster living in the map's own rotated ground-meter frame
/// (`local_width`/`local_height`/`local_origin`/`local_px_size` --
/// `ContourRaster`'s own convention: `local_origin` is pixel `(0, 0)`'s own
/// corner, not its center) onto a plain axis-aligned grid in `georef`'s own
/// projected CRS. Walks every *destination* pixel and asks `sample` for its
/// value at that pixel's own local-frame position (via
/// [`projected_to_ground`]), rather than scattering the source raster's own
/// pixels forward -- so every output pixel is filled exactly once and a
/// grivated source can't leave gaps a forward splat would between its own
/// rotated pixel corners. `sample` reads the source raster at any continuous
/// local coordinate (bilinear or otherwise -- this module has no opinion),
/// returning `f32::NAN` outside it.
///
/// Output pixel size is `local_px_size * georef.auxiliary_scale_factor` --
/// `ground_to_projected`'s own ground-meter-to-projected-meter scale -- so a
/// zero-grivation map resamples 1:1 rather than gratuitously blurring.
///
/// `None` where [`ground_to_projected`] itself would be (`georef.epsg ==
/// 0`).
pub fn resample_north_up(
    local_width: usize,
    local_height: usize,
    local_origin: Coord<f64>,
    local_px_size: f64,
    sample: impl Fn(Coord<f64>) -> f32,
    georef: &Georeferencing,
) -> Option<NorthUpRaster> {
    // All four corners: a grivated footprint's projected bounding box isn't
    // pinned down by any single corner pair.
    let corners = [
        Coord {
            x: local_origin.x,
            y: local_origin.y,
        },
        Coord {
            x: local_origin.x + local_width as f64 * local_px_size,
            y: local_origin.y,
        },
        Coord {
            x: local_origin.x,
            y: local_origin.y + local_height as f64 * local_px_size,
        },
        Coord {
            x: local_origin.x + local_width as f64 * local_px_size,
            y: local_origin.y + local_height as f64 * local_px_size,
        },
    ];
    let mut min = Coord {
        x: f64::INFINITY,
        y: f64::INFINITY,
    };
    let mut max = Coord {
        x: f64::NEG_INFINITY,
        y: f64::NEG_INFINITY,
    };
    for corner in corners {
        let p = ground_to_projected(georef, corner)?;
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }

    let px_size = local_px_size * georef.auxiliary_scale_factor;
    let width = (((max.x - min.x) / px_size).round() as usize).max(1);
    let height = (((max.y - min.y) / px_size).round() as usize).max(1);

    let mut data = vec![f32::NAN; width * height];
    for row in 0..height {
        let northing = max.y - (row as f64 + 0.5) * px_size;
        for col in 0..width {
            let easting = min.x + (col as f64 + 0.5) * px_size;
            if let Some(local) = projected_to_ground(
                georef,
                Coord {
                    x: easting,
                    y: northing,
                },
            ) {
                data[row * width + col] = sample(local);
            }
        }
    }

    Some(NorthUpRaster {
        width,
        height,
        origin: Coord { x: min.x, y: max.y },
        px_size,
        data,
    })
}

/// The raster's own pixel-corner-to-projected affine, as a row-major 4x4
/// matrix (`GeoTIFF`'s own `ModelTransformationTag` layout): raster pixel
/// `(I, J, 0, 1)` (the *corner* of pixel `(I, J)` -- matching
/// [`geo_key_directory`]'s own `GTRasterTypeGeoKeyRasterPixelIsArea`, not its
/// center) maps to `(easting, northing, 0, 1)` under `result * (I, J, 0,
/// 1)ᵀ`. `origin`/`px_size` are `ContourRaster`'s own (`raster.origin`,
/// `raster.px_size`) -- `origin` is already pixel `(0, 0)`'s own corner, not
/// its center, since [`crate::contour_raster::ContourRaster::to_px`] floors
/// rather than rounds. `None` wherever [`ground_to_projected`] itself would
/// be (`georef.epsg == 0`).
///
/// Elevation itself is left alone -- row 3 is the identity's own `(0, 0, 1,
/// 0)` -- since a DEM's own values are already only meaningful up to a
/// constant (`Contours-to-Raster.md`'s opening line); there is no vertical
/// datum here to place them against either way.
pub fn model_transformation(
    georef: &Georeferencing,
    origin: Coord<f64>,
    px_size: f64,
) -> Option<[f64; 16]> {
    let p00 = ground_to_projected(georef, origin)?;
    let p10 = ground_to_projected(
        georef,
        Coord {
            x: origin.x + px_size,
            y: origin.y,
        },
    )?;
    let p01 = ground_to_projected(
        georef,
        Coord {
            x: origin.x,
            y: origin.y + px_size,
        },
    )?;
    Some([
        p10.x - p00.x,
        p01.x - p00.x,
        0.0,
        p00.x,
        p10.y - p00.y,
        p01.y - p00.y,
        0.0,
        p00.y,
        0.0,
        0.0,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ])
}

/// GeoTIFF key IDs this module writes -- see the GeoTIFF 1.0/1.1 spec's own
/// registry for what every other key would mean; these four are the whole
/// set a projected, linear-unit, pixel-is-area raster needs.
const GT_MODEL_TYPE_GEO_KEY: u16 = 1024;
const GT_RASTER_TYPE_GEO_KEY: u16 = 1025;
const PROJECTED_CS_TYPE_GEO_KEY: u16 = 3072;
const PROJ_LINEAR_UNITS_GEO_KEY: u16 = 3076;

const MODEL_TYPE_PROJECTED: u16 = 1;
const RASTER_PIXEL_IS_AREA: u16 = 1;
const LINEAR_UNIT_METRE: u16 = 9001;

/// A `GeoKeyDirectoryTag` (GeoTIFF tag `34735`) naming `epsg`'s own
/// projected CRS, in meters, over pixel-is-area pixels -- the GeoTIFF 1.0
/// spec's own header-plus-4-shorts-per-key layout, built by hand since
/// nothing in this crate's own dependencies generates one. Every key here
/// stores its own value directly in the entry's own fourth short (a `SHORT`
/// key with `TIFFTagLocation = 0`), so there is no second tag (a
/// `GeoDoubleParamsTag` or `GeoAsciiParamsTag`) for any of this to point
/// into.
pub fn geo_key_directory(epsg: u16) -> Vec<u16> {
    vec![
        // Header: KeyDirectoryVersion, KeyRevision, MinorRevision, NumberOfKeys.
        1,
        1,
        0,
        4,
        // KeyID, TIFFTagLocation, Count, Value_Offset -- one row per key.
        GT_MODEL_TYPE_GEO_KEY,
        0,
        1,
        MODEL_TYPE_PROJECTED,
        GT_RASTER_TYPE_GEO_KEY,
        0,
        1,
        RASTER_PIXEL_IS_AREA,
        PROJECTED_CS_TYPE_GEO_KEY,
        0,
        1,
        epsg,
        PROJ_LINEAR_UNITS_GEO_KEY,
        0,
        1,
        LINEAR_UNIT_METRE,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    /// The exact fixture `tests/snap.rs`'s own
    /// `a_georeferenced_map_says_where_it_is_and_which_way_it_points` test
    /// already checks the raw XML parses into -- and `no_public_maps/17_Skatas_stamp.omap`'s
    /// own real `<georeferencing>` (a real Mapper export), give or take that
    /// file's own `auxiliary_scale_factor` (`1.000014`).
    fn skatas_georef() -> Georeferencing {
        Georeferencing {
            scale: 15000,
            epsg: 3006,
            ref_point_x: 322500.0,
            ref_point_y: 6397500.0,
            grivation: 7.1,
            grivation_specified: true,
            auxiliary_scale_factor: 1.000014,
        }
    }

    #[test]
    fn no_epsg_means_no_projected_coordinate() {
        let georef = Georeferencing::default();
        assert_eq!(ground_to_projected(&georef, c(1.0, 1.0)), None);
    }

    #[test]
    fn the_map_origin_always_lands_exactly_on_the_reference_point() {
        // (0, 0) is a fixed point of the rotation regardless of grivation or
        // scale, so it must land exactly on ref_point no matter what either
        // is -- this is what actually pins the reference point down, unlike
        // any other point.
        let georef = skatas_georef();
        let p = ground_to_projected(&georef, c(0.0, 0.0)).unwrap();
        assert!((p.x - georef.ref_point_x).abs() < 1e-9);
        assert!((p.y - georef.ref_point_y).abs() < 1e-9);
    }

    #[test]
    fn zero_grivation_is_a_plain_north_up_scale() {
        // With no rotation, paper-right must be projected-east and
        // paper-down must be projected-south (ContourRaster's own y grows
        // downward, same as the map's paper convention) -- i.e. a pure
        // scale with the y axis flipped, nothing else.
        let georef = Georeferencing {
            grivation: 0.0,
            auxiliary_scale_factor: 2.0,
            ..skatas_georef()
        };
        let p = ground_to_projected(&georef, c(10.0, 10.0)).unwrap();
        assert!((p.x - (georef.ref_point_x + 20.0)).abs() < 1e-9);
        assert!((p.y - (georef.ref_point_y - 20.0)).abs() < 1e-9);
    }

    #[test]
    fn grivation_rotates_the_ground_frame_rigidly() {
        // Whatever the exact sign convention, a nonzero grivation must still
        // preserve distance from the reference point (it's a rotation, not
        // a shear) -- this doesn't pin the sign down, but it does catch a
        // formula that accidentally stretches instead of only rotating.
        let georef = skatas_georef();
        let local = c(37.0, -12.0);
        let p = ground_to_projected(&georef, local).unwrap();
        let local_dist = local.x.hypot(local.y);
        let proj_dist = (p.x - georef.ref_point_x).hypot(p.y - georef.ref_point_y);
        assert!(
            (local_dist * georef.auxiliary_scale_factor - proj_dist).abs() < 1e-6,
            "expected a rigid (scaled) rotation, local_dist={local_dist} proj_dist={proj_dist}"
        );
    }

    #[test]
    fn model_transformation_agrees_with_ground_to_projected_at_every_corner_it_uses() {
        let georef = skatas_georef();
        let origin = c(100.0, 200.0);
        let px_size = 0.5;
        let m = model_transformation(&georef, origin, px_size).unwrap();

        let apply = |i: f64, j: f64| Coord {
            x: m[0] * i + m[1] * j + m[3],
            y: m[4] * i + m[5] * j + m[7],
        };

        let expect = |local: Coord<f64>| ground_to_projected(&georef, local).unwrap();

        let p00 = apply(0.0, 0.0);
        let want00 = expect(origin);
        assert!((p00.x - want00.x).abs() < 1e-9 && (p00.y - want00.y).abs() < 1e-9);

        let p10 = apply(1.0, 0.0);
        let want10 = expect(c(origin.x + px_size, origin.y));
        assert!((p10.x - want10.x).abs() < 1e-9 && (p10.y - want10.y).abs() < 1e-9);

        let p01 = apply(0.0, 1.0);
        let want01 = expect(c(origin.x, origin.y + px_size));
        assert!((p01.x - want01.x).abs() < 1e-9 && (p01.y - want01.y).abs() < 1e-9);
    }

    #[test]
    fn model_transformation_is_none_without_an_epsg() {
        let georef = Georeferencing::default();
        assert!(model_transformation(&georef, c(0.0, 0.0), 1.0).is_none());
    }

    #[test]
    fn geo_key_directory_has_the_header_and_one_row_per_key() {
        let keys = geo_key_directory(3006);
        assert_eq!(keys.len(), 4 + 4 * 4, "header plus 4 keys, 4 shorts each");
        assert_eq!(
            &keys[0..4],
            &[1, 1, 0, 4],
            "GeoKeyDirectoryTag's own header"
        );
        assert_eq!(
            &keys[4..8],
            &[GT_MODEL_TYPE_GEO_KEY, 0, 1, MODEL_TYPE_PROJECTED]
        );
        assert_eq!(
            &keys[8..12],
            &[GT_RASTER_TYPE_GEO_KEY, 0, 1, RASTER_PIXEL_IS_AREA]
        );
        assert_eq!(&keys[12..16], &[PROJECTED_CS_TYPE_GEO_KEY, 0, 1, 3006]);
        assert_eq!(
            &keys[16..20],
            &[PROJ_LINEAR_UNITS_GEO_KEY, 0, 1, LINEAR_UNIT_METRE]
        );
    }

    #[test]
    fn projected_to_ground_is_none_without_an_epsg() {
        let georef = Georeferencing::default();
        assert!(projected_to_ground(&georef, c(0.0, 0.0)).is_none());
    }

    #[test]
    fn projected_to_ground_undoes_ground_to_projected() {
        let georef = skatas_georef();
        for local in [c(0.0, 0.0), c(37.0, -12.0), c(-500.5, 320.0)] {
            let projected = ground_to_projected(&georef, local).unwrap();
            let back = projected_to_ground(&georef, projected).unwrap();
            assert!((back.x - local.x).abs() < 1e-6 && (back.y - local.y).abs() < 1e-6);
        }
    }

    #[test]
    fn resample_north_up_is_none_without_an_epsg() {
        let georef = Georeferencing::default();
        assert!(resample_north_up(2, 2, c(0.0, 0.0), 1.0, |_| 0.0, &georef).is_none());
    }

    #[test]
    fn resample_north_up_recovers_the_source_value_everywhere_at_zero_grivation() {
        // With no rotation, every destination pixel's own inverse-mapped
        // local coordinate must land back inside the source footprint, so a
        // source that reads its own local `x` back verbatim must come back
        // unchanged (up to the pixel-center rounding the resample itself
        // does).
        let georef = Georeferencing {
            grivation: 0.0,
            ..skatas_georef()
        };
        let raster = resample_north_up(10, 10, c(0.0, 0.0), 1.0, |local| local.x as f32, &georef)
            .unwrap();
        assert_eq!(raster.width, 10);
        assert_eq!(raster.height, 10);
        assert!(raster.data.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn resample_north_up_leaves_no_rotation_for_a_naive_reader_to_drop() {
        // The whole point of resampling before writing: a plain
        // axis-aligned pixel size/origin, not a rotated affine, is what
        // comes out even from a grivated source.
        let georef = skatas_georef(); // grivation: 7.1
        let raster = resample_north_up(20, 20, c(0.0, 0.0), 1.0, |_| 1.0, &georef).unwrap();
        assert!(raster.px_size > 0.0);
        // A grivated footprint's axis-aligned bounding box is strictly
        // larger than the source's own square footprint.
        assert!(raster.width * raster.height > 20 * 20);
    }
}
