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
}
