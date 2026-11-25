//! What to ask an elevation server for, so a map gets the terrain model
//! (DTM) of its own ground as a GeoTIFF: what `mauro terrain` downloads, and
//! the ground truth `mauro dem-eval` scores `mauro dem`'s contours against.
//!
//! Like [`orthophoto`](crate::orthophoto), nothing here touches the network:
//! this module says which URLs to fetch, puts what comes back together and
//! writes the result, and the `mauro terrain` tool does the fetching. A
//! [`TerrainService`] is one elevation layer of one server, and
//! [`TERRAIN_SERVICES`](crate::terrain_services::TERRAIN_SERVICES) lists
//! every one this crate knows of, best first.
//!
//! Two kinds of server are spoken to ([`Protocol`]), both able to hand out
//! any box, in any CRS, at any size, as a float TIFF:
//!
//! - an ArcGIS `ImageServer`, by its `exportImage` operation (the region of
//!   Emilia-Romagna's LiDAR surveys);
//! - an OGC WCS 2.0.1 server with the CRS and scaling extensions, by
//!   `GetCoverage` (GeoServer: INGV's TINITALY, all of Italy).
//!
//! The steps are:
//!
//! 1. [`map_footprint`](crate::orthophoto::map_footprint): the box the map's
//!    objects cover, in the map's own projected CRS. The terrain is always
//!    asked for in that CRS, so it lands on the map with no reprojecting on
//!    this side, and `mauro dem-eval` can compare it with `mauro dem`'s
//!    output, which is in it too.
//! 2. [`Grid::covering`]: that box as a north-up pixel grid, cut into
//!    [`Grid::tiles`] of at most [`tile_size`] pixels, and the request for
//!    each: what to ask for ([`request_area`]) and how ([`coverage_url`]).
//! 3. [`ElevationGrid::paste`]: each tile read back
//!    ([`read_bytes`](crate::dem_geo_tiff_evaluator::read_bytes)) and
//!    sampled at the grid's own pixel centres through the tile's *own*
//!    georeferencing, not the one asked for: a WCS server that reprojects
//!    lays its output grid where it likes, a few meters and a fraction of a
//!    percent of a pixel off the request.
//! 4. [`ElevationGrid::write_geotiff`]: the whole grid as a single-band
//!    float GeoTIFF, [`NO_DATA`] where no service had a value.

use std::io::{Seek, Write};

use tiff::encoder::colortype::Gray32Float;
use tiff::encoder::compression::DeflateLevel;
use tiff::encoder::{Compression, TiffEncoder};
use tiff::tags::Tag;

use crate::dem_geo_tiff_evaluator::GeoRaster;
use crate::geotiff::geo_key_directory;
use crate::orthophoto::{Grid, LatLonBox, ProjectedBox, Tile};

/// How a server is asked for elevations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Protocol {
    /// An ArcGIS `ImageServer` (its URL ends in `/ImageServer`), asked by
    /// `exportImage` for a float TIFF.
    ArcGisImageServer,
    /// An OGC WCS 2.0.1 server (its URL ends before the `?`), asked by
    /// `GetCoverage` for one coverage.
    Wcs {
        /// The coverage's `CoverageId`.
        coverage_id: &'static str,
    },
}

/// One elevation layer of one server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerrainService {
    /// A short name to pick it by: `it-er-lidar-2023-24`.
    pub id: &'static str,
    /// The ground it covers, for people.
    pub area: &'static str,
    /// When the ground was surveyed: "2023–2024".
    pub years: &'static str,
    /// The endpoint (see [`Protocol`] for where it ends).
    pub url: &'static str,
    /// How to ask it.
    pub protocol: Protocol,
    /// The model's own ground resolution, in meters per pixel.
    pub ground_resolution: f64,
    /// The largest image, in pixels a side, to ask for in one request.
    pub max_tile_size: u32,
    /// Roughly where it has elevations: a box around them. Where the box is
    /// larger than the survey, a coverage probe tells the rest apart.
    pub coverage: LatLonBox,
    /// Who to credit for the elevations.
    pub attribution: &'static str,
    /// The terms they are published under.
    pub licence: &'static str,
}

impl TerrainService {
    /// Whether `lat`, `lon` is inside the service's
    /// [`coverage`](Self::coverage) box: worth asking, not yet a promise of
    /// elevations.
    pub fn covers(&self, lat: f64, lon: f64) -> bool {
        self.coverage.contains(lat, lon)
    }
}

/// The value written where there is no elevation, and asked of a server
/// that lets one choose (ArcGIS).
pub const NO_DATA: f32 = -9999.0;

/// The axis labels a WCS may want a subset of a projected CRS named by,
/// easting first, in the order worth trying: GeoServer takes them from the
/// EPSG registry's abbreviations, `E`/`N` for UTM, `X`/`Y` for Gauss-Boaga
/// (EPSG:3003) and Web Mercator, and turns any other away as "Invalid axis
/// label" ([`is_axis_label_error`]).
pub const WCS_AXES: &[(&str, &str)] = &[("E", "N"), ("X", "Y"), ("x", "y")];

/// The OGC URI of EPSG code `epsg`, as WCS 2.0 names a CRS.
fn crs_uri(epsg: i32) -> String {
    format!("http://www.opengis.net/def/crs/EPSG/0/{epsg}")
}

/// What to ask `service` for, to fill `area` with `width` by `height`
/// pixels: the box and the size of the raster to request.
///
/// An ArcGIS server lays its output exactly on the grid asked for and
/// resamples the model onto it itself, so that is what it is asked for:
/// uncompressed (its LZW streams end without the end code a decoder wants),
/// and by nearest neighbour (its bilinear blends a survey's edge with the
/// no-data value, into heights of hundreds of meters below sea level). A
/// WCS server (GeoServer) does neither: it snaps the box to its own pixels
/// before scaling, and its linear interpolation leaves the last row and
/// column empty. So it is asked for its model's own pixels, over the box
/// grown by two of them on every side, and [`ElevationGrid::paste`] does the
/// resampling, through the georeferencing the server sent.
pub fn request_area(
    service: &TerrainService,
    area: &ProjectedBox,
    width: u32,
    height: u32,
) -> (ProjectedBox, u32, u32) {
    match service.protocol {
        Protocol::ArcGisImageServer => (*area, width, height),
        Protocol::Wcs { .. } => {
            let native = service.ground_resolution;
            let grown = ProjectedBox {
                min_x: area.min_x - 2.0 * native,
                min_y: area.min_y - 2.0 * native,
                max_x: area.max_x + 2.0 * native,
                max_y: area.max_y + 2.0 * native,
            };
            let side = |extent: f64| (extent / native).ceil().max(1.0) as u32;
            (grown, side(grown.width()), side(grown.height()))
        }
    }
}

/// The largest tile, in pixels a side of `pixel` meters, to cut a grid into
/// for `service`, so that what is requested for one ([`request_area`]) is
/// never larger than its [`max_tile_size`](TerrainService::max_tile_size).
pub fn tile_size(service: &TerrainService, pixel: f64) -> u32 {
    match service.protocol {
        Protocol::ArcGisImageServer => service.max_tile_size,
        Protocol::Wcs { .. } => {
            let max = f64::from(service.max_tile_size) - 5.0;
            (max * (service.ground_resolution / pixel).min(1.0))
                .floor()
                .max(1.0) as u32
        }
    }
}

/// The request for `area` (in EPSG `epsg`) from `service`, as a `width` by
/// `height` float TIFF of the model's nearest pixels (see [`request_area`] for what to give it). `axes`
/// are the easting and northing labels to subset a WCS by (one of
/// [`WCS_AXES`]); an ArcGIS server ignores them.
pub fn coverage_url(
    service: &TerrainService,
    epsg: i32,
    area: &ProjectedBox,
    width: u32,
    height: u32,
    (east, north): (&str, &str),
) -> String {
    match service.protocol {
        Protocol::ArcGisImageServer => format!(
            "{}/exportImage?bbox={},{},{},{}&bboxSR={epsg}&imageSR={epsg}&size={width},{height}\
             &format=tiff&pixelType=F32&noData={NO_DATA}\
             &interpolation=RSP_NearestNeighbor&f=image",
            service.url, area.min_x, area.min_y, area.max_x, area.max_y,
        ),
        Protocol::Wcs { coverage_id } => {
            let crs = crs_uri(epsg);
            format!(
                "{}?SERVICE=WCS&VERSION=2.0.1&REQUEST=GetCoverage&COVERAGEID={coverage_id}\
                 &FORMAT=image/tiff&SUBSETTINGCRS={crs}&OUTPUTCRS={crs}\
                 &SUBSET={east}({},{})&SUBSET={north}({},{})&SCALESIZE=i({width}),j({height})",
                service.url, area.min_x, area.max_x, area.min_y, area.max_y,
            )
        }
    }
}

/// What a server's error says, where `body` is one: an OGC exception
/// report's `<ows:ExceptionText>` (or `<ServiceException>`), an ArcGIS JSON
/// `"message"`, else the start of the text itself.
pub fn error_text(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let between = |open: &str, close: &str| -> Option<String> {
        let start = text.find(open)? + open.len();
        let start = start + text[start..].find('>')? + 1;
        let end = start + text[start..].find(close)?;
        Some(text[start..end].trim().to_owned())
    };
    let json_message = || -> Option<String> {
        let key = "\"message\":\"";
        let start = text.find(key)? + key.len();
        let end = start + text[start..].find('"')?;
        Some(text[start..end].to_owned())
    };
    between("<ows:ExceptionText", "</ows:ExceptionText>")
        .or_else(|| between("<ServiceException", "</ServiceException>"))
        .or_else(json_message)
        .unwrap_or_else(|| {
            let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            flat.chars().take(300).collect()
        })
}

/// Whether a WCS error ([`error_text`]) says the subset's axis labels are
/// not the CRS's: try the next pair of [`WCS_AXES`].
pub fn is_axis_label_error(text: &str) -> bool {
    text.to_ascii_lowercase().contains("axis label")
}

/// Whether a WCS error ([`error_text`]) says the box is wholly outside the
/// coverage: no elevations there, rather than a failure.
pub fn is_outside_error(text: &str) -> bool {
    text.to_ascii_lowercase().contains("empty intersection")
}

/// Whether a value a server sent is an elevation, not a stand-in for none:
/// [`NO_DATA`], `NaN`, and ArcGIS's lowest float all fall outside the range
/// any ground on the earth is in.
pub fn is_elevation(value: f64) -> bool {
    value.is_finite() && (-1000.0..9000.0).contains(&value)
}

/// A north-up elevation raster on a [`Grid`], filled tile by tile.
#[derive(Clone, Debug)]
pub struct ElevationGrid {
    /// Where it is, in the CRS it was asked in.
    pub grid: Grid,
    /// Elevations, row by row from the north-west corner; `NaN` where there
    /// is none (yet).
    pub values: Vec<f32>,
}

impl ElevationGrid {
    /// An empty raster on `grid`: no elevation anywhere.
    pub fn new(grid: Grid) -> Self {
        Self {
            values: vec![f32::NAN; grid.pixel_count() as usize],
            grid,
        }
    }

    /// Fills `tile` of the grid from `raster`, which a server sent for it:
    /// every pixel the value `raster` has at that pixel's centre, through
    /// `raster`'s own georeferencing ([`GeoRaster::sample`]), so a server
    /// that laid its grid a little off the one asked for still lands right.
    /// A pixel `raster` has no elevation at ([`is_elevation`]) is left as it
    /// was.
    pub fn paste(&mut self, tile: &Tile, raster: &GeoRaster) {
        let g = self.grid;
        for row in tile.y..(tile.y + tile.height).min(g.height) {
            let y = g.north - (f64::from(row) + 0.5) * g.pixel_height;
            for col in tile.x..(tile.x + tile.width).min(g.width) {
                let x = g.west + (f64::from(col) + 0.5) * g.pixel_width;
                if let Some(v) = raster.sample(x, y).filter(|&v| is_elevation(v)) {
                    self.values[row as usize * g.width as usize + col as usize] = v as f32;
                }
            }
        }
    }

    /// The share of the grid with an elevation, from 0 to 1.
    pub fn coverage(&self) -> f64 {
        let known = self.values.iter().filter(|v| !v.is_nan()).count();
        known as f64 / self.values.len().max(1) as f64
    }

    /// The lowest and highest elevation, where there is any.
    pub fn range(&self) -> Option<(f32, f32)> {
        self.values
            .iter()
            .filter(|v| !v.is_nan())
            .fold(None, |range, &v| match range {
                None => Some((v, v)),
                Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
            })
    }

    /// Writes the raster as a single-band, deflate-compressed float GeoTIFF
    /// in EPSG `epsg`: north-up (`ModelPixelScaleTag` and
    /// `ModelTiepointTag`, which every GIS reads), [`NO_DATA`] (declared as
    /// its `GDAL_NODATA`) where there is no elevation.
    pub fn write_geotiff<W: Write + Seek>(&self, writer: W, epsg: u16) -> Result<(), String> {
        const MODEL_PIXEL_SCALE_TAG: u16 = 33550;
        const MODEL_TIEPOINT_TAG: u16 = 33922;
        const GEO_KEY_DIRECTORY_TAG: u16 = 34735;
        const GDAL_NODATA_TAG: u16 = 42113;
        let g = &self.grid;
        let fail = |e: tiff::TiffError| e.to_string();
        let mut encoder = TiffEncoder::new(writer)
            .map_err(fail)?
            .with_compression(Compression::Deflate(DeflateLevel::Balanced));
        let mut image = encoder
            .new_image::<Gray32Float>(g.width, g.height)
            .map_err(fail)?;
        let tags = image.encoder();
        tags.write_tag(Tag::Unknown(GDAL_NODATA_TAG), &*NO_DATA.to_string())
            .map_err(fail)?;
        tags.write_tag(
            Tag::Unknown(MODEL_PIXEL_SCALE_TAG),
            &[g.pixel_width, g.pixel_height, 0.0][..],
        )
        .map_err(fail)?;
        tags.write_tag(
            Tag::Unknown(MODEL_TIEPOINT_TAG),
            &[0.0, 0.0, 0.0, g.west, g.north, 0.0][..],
        )
        .map_err(fail)?;
        tags.write_tag(
            Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
            &geo_key_directory(epsg)[..],
        )
        .map_err(fail)?;
        let data: Vec<f32> = self
            .values
            .iter()
            .map(|&v| if v.is_nan() { NO_DATA } else { v })
            .collect();
        image.write_data(&data).map_err(fail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem_geo_tiff_evaluator::{read_bytes, CrsKey};

    fn arcgis() -> TerrainService {
        TerrainService {
            id: "test-arcgis",
            area: "Testland",
            years: "2024",
            url: "https://example.org/arcgis/rest/services/Dtm/ImageServer",
            protocol: Protocol::ArcGisImageServer,
            ground_resolution: 0.5,
            max_tile_size: 2000,
            coverage: LatLonBox {
                min_lon: 10.0,
                min_lat: 44.0,
                max_lon: 11.0,
                max_lat: 45.0,
            },
            attribution: "Test",
            licence: "CC BY 4.0",
        }
    }

    fn area() -> ProjectedBox {
        ProjectedBox {
            min_x: 680000.0,
            min_y: 4918000.0,
            max_x: 681000.0,
            max_y: 4919000.0,
        }
    }

    #[test]
    fn an_arcgis_request_names_the_box_crs_and_size() {
        let url = coverage_url(&arcgis(), 32632, &area(), 200, 100, WCS_AXES[0]);
        assert!(url
            .starts_with("https://example.org/arcgis/rest/services/Dtm/ImageServer/exportImage?"));
        assert!(url.contains("bbox=680000,4918000,681000,4919000&bboxSR=32632&imageSR=32632"));
        assert!(url.contains("&size=200,100&"));
        assert!(url.contains("pixelType=F32") && url.contains("noData=-9999"));
    }

    #[test]
    fn a_wcs_request_subsets_by_the_axes_given_and_scales_to_the_size() {
        let service = TerrainService {
            url: "https://example.org/wcs",
            protocol: Protocol::Wcs { coverage_id: "dem" },
            ..arcgis()
        };
        let url = coverage_url(&service, 3003, &area(), 200, 100, ("X", "Y"));
        assert!(url
            .starts_with("https://example.org/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=GetCoverage"));
        assert!(url.contains("COVERAGEID=dem"));
        assert!(url.contains("OUTPUTCRS=http://www.opengis.net/def/crs/EPSG/0/3003"));
        assert!(url.contains("SUBSET=X(680000,681000)&SUBSET=Y(4918000,4919000)"));
        assert!(url.contains("SCALESIZE=i(200),j(100)"));
    }

    #[test]
    fn an_arcgis_server_is_asked_for_the_grid_itself() {
        assert_eq!(
            request_area(&arcgis(), &area(), 200, 100),
            (area(), 200, 100)
        );
        assert_eq!(tile_size(&arcgis(), 0.1), 2000);
        assert_eq!(tile_size(&arcgis(), 50.0), 2000);
    }

    #[test]
    fn a_wcs_is_asked_for_its_own_pixels_two_wider_every_side() {
        let service = TerrainService {
            protocol: Protocol::Wcs { coverage_id: "dem" },
            ground_resolution: 10.0,
            ..arcgis()
        };
        let (grown, width, height) = request_area(&service, &area(), 400, 400);
        assert_eq!(grown.min_x, 679980.0);
        assert_eq!(grown.max_y, 4919020.0);
        assert_eq!((width, height), (104, 104));
        // Pixels coarser than the model's: tiles shrink so that the
        // model's own pixels over one, grown, stay within the limit.
        assert_eq!(tile_size(&service, 5.0), 1995);
        let tile = tile_size(&service, 40.0);
        assert_eq!(tile, 498);
        let side = f64::from(tile) * 40.0;
        let (_, w, _) = request_area(
            &service,
            &ProjectedBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: side,
                max_y: side,
            },
            tile,
            tile,
        );
        assert!(w <= service.max_tile_size, "{w}");
    }

    #[test]
    fn errors_are_read_from_ogc_reports_and_arcgis_json() {
        let ogc = br#"<?xml version="1.0"?><ows:ExceptionReport><ows:Exception exceptionCode="InvalidSubsetting"><ows:ExceptionText>Invalid axis label provided: E</ows:ExceptionText></ows:Exception></ows:ExceptionReport>"#;
        assert_eq!(error_text(ogc), "Invalid axis label provided: E");
        assert!(is_axis_label_error(&error_text(ogc)));
        let arcgis = br#"{"error":{"code":400,"message":"Invalid or missing input parameters.","details":[]}}"#;
        assert_eq!(error_text(arcgis), "Invalid or missing input parameters.");
        assert!(!is_axis_label_error(&error_text(arcgis)));
        let outside = b"<ows:ExceptionReport><ows:Exception exceptionCode=\"InvalidSubsetting\">\n<ows:ExceptionText>Empty intersection after subsetting</ows:ExceptionText></ows:Exception></ows:ExceptionReport>";
        assert!(is_outside_error(&error_text(outside)));
        assert!(!is_axis_label_error(&error_text(outside)));
    }

    #[test]
    fn no_data_stand_ins_are_not_elevations() {
        assert!(is_elevation(0.0) && is_elevation(-50.0) && is_elevation(4808.0));
        assert!(!is_elevation(f64::from(NO_DATA)));
        assert!(!is_elevation(f64::NAN));
        assert!(!is_elevation(f64::from(f32::MIN)));
    }

    /// A grid of 10 m pixels over `area()`, 100 × 100.
    fn grid() -> Grid {
        Grid::covering(&area(), (10.0, 10.0))
    }

    #[test]
    fn a_written_geotiff_reads_back_where_it_was_with_its_gaps() {
        let mut dem = ElevationGrid::new(grid());
        for (i, v) in dem.values.iter_mut().enumerate() {
            *v = i as f32 / 10.0;
        }
        dem.values[42] = f32::NAN;
        let mut bytes = std::io::Cursor::new(Vec::new());
        dem.write_geotiff(&mut bytes, 32632).unwrap();
        let back = read_bytes(bytes.get_ref(), "test").unwrap();
        assert_eq!((back.width, back.height), (100, 100));
        assert_eq!(back.crs, CrsKey::Projected(32632));
        assert_eq!(
            back.transform.pixel_to_world(0.0, 0.0),
            (680000.0, 4919000.0)
        );
        assert_eq!(
            back.transform.pixel_to_world(100.0, 100.0),
            (681000.0, 4918000.0)
        );
        assert_eq!(back.get(41, 0), Some(4.099999904632568));
        assert_eq!(back.get(42, 0), None);
        assert_eq!(back.get(99, 99), Some(f64::from(9999.0f32 / 10.0)));
    }

    /// A server's tile, as [`read_bytes`] reads one back: `values` on a grid
    /// of `pixel` meters from `west`, `north`.
    fn tile_raster(
        west: f64,
        north: f64,
        pixel: f64,
        side: usize,
        value: impl Fn(f64, f64) -> f64,
    ) -> GeoRaster {
        let mut values = Vec::new();
        for row in 0..side {
            for col in 0..side {
                let x = west + (col as f64 + 0.5) * pixel;
                let y = north - (row as f64 + 0.5) * pixel;
                values.push(value(x, y));
            }
        }
        let mut dem = ElevationGrid::new(Grid {
            west,
            north,
            pixel_width: pixel,
            pixel_height: pixel,
            width: side as u32,
            height: side as u32,
        });
        dem.values = values.iter().map(|&v| v as f32).collect();
        let mut bytes = std::io::Cursor::new(Vec::new());
        dem.write_geotiff(&mut bytes, 32632).unwrap();
        read_bytes(bytes.get_ref(), "tile").unwrap()
    }

    #[test]
    fn a_tile_on_the_grid_is_pasted_value_for_value() {
        let mut dem = ElevationGrid::new(grid());
        let tile = Tile {
            x: 50,
            y: 0,
            width: 50,
            height: 50,
        };
        let raster = tile_raster(680500.0, 4919000.0, 10.0, 50, |x, y| {
            (x - 680000.0) + (y - 4918000.0) / 1000.0
        });
        dem.paste(&tile, &raster);
        assert!((dem.coverage() - 0.25).abs() < 1e-9);
        // Column 50, row 0: the centre (680505, 4918995).
        assert!((dem.values[50] - 505.995).abs() < 1e-3);
        assert!(dem.values[49].is_nan() && dem.values[50 * 100 + 50].is_nan());
    }

    #[test]
    fn a_tile_laid_off_the_grid_is_sampled_where_the_pixels_are() {
        // A plane, sent on a grid 2.5 m off and a little coarser, as a
        // reprojecting WCS lays it: the plane comes back exactly.
        let plane = |x: f64, y: f64| 100.0 + 0.01 * (x - 680000.0) - 0.02 * (y - 4918000.0);
        let raster = tile_raster(679997.5, 4919002.5, 10.001, 102, plane);
        let mut dem = ElevationGrid::new(grid());
        dem.paste(
            &Tile {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            &raster,
        );
        assert_eq!(dem.coverage(), 1.0);
        for (row, col) in [(0u32, 0u32), (37, 81), (99, 99)] {
            let x = 680000.0 + (f64::from(col) + 0.5) * 10.0;
            let y = 4919000.0 - (f64::from(row) + 0.5) * 10.0;
            let got = f64::from(dem.values[(row * 100 + col) as usize]);
            assert!((got - plane(x, y)).abs() < 1e-3, "{row},{col}: {got}");
        }
    }

    #[test]
    fn no_data_in_a_tile_leaves_the_grid_as_it_was() {
        let raster = tile_raster(680000.0, 4919000.0, 10.0, 100, |_, _| f64::from(NO_DATA));
        let mut dem = ElevationGrid::new(grid());
        dem.values[0] = 7.0;
        dem.paste(
            &Tile {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            &raster,
        );
        assert_eq!(dem.values[0], 7.0);
        assert!(dem.values[1].is_nan());
        assert_eq!(dem.range(), Some((7.0, 7.0)));
    }
}
