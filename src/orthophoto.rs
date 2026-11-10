//! What to ask an orthophoto server for, so a map can be drawn over the
//! aerial photo of its own ground.
//!
//! Italy's orthophotos are flown by AGEA (Agenzia per le Erogazioni in
//! Agricoltura) every few years, region by region, and served by each region
//! rather than nationally. Every region publishes its own WMS, so a
//! [`Service`] is one region's one flight. Only Emilia-Romagna's are listed
//! for now ([`EMILIA_ROMAGNA`]).
//!
//! Nothing here touches the network: this module says which URLs to fetch and
//! where the result lands, and the `add_orthophoto` tool does the fetching.
//! The steps are:
//!
//! 1. [`map_footprint`]: the box a map's objects cover, plus a margin, in
//!    the map's own projected CRS. It is the axis-aligned box *around* the
//!    map, since a map turned by its grivation is not axis-aligned itself.
//! 2. [`Grid::covering`]: that box as a pixel grid, its edges on whole
//!    multiples of the pixel size.
//! 3. [`Grid::tiles`] and [`get_map_url`]: the grid in pieces no larger than
//!    a server hands out in one request, and the `GetMap` request for each.
//! 4. [`Grid::world_file`]: where the stitched image is on the earth, as the
//!    world file Mapper reads beside it (`src/templates/world_file.cpp`).
//!
//! Requests use WMS 1.1.1, whose `SRS` bounding box is always easting first.
//! In 1.3.0 the axis order is whatever the EPSG registry says for that CRS,
//! and some Italian ones (RDN2008's "N-E" UTM, EPSG:7791) are northing first.

use crate::geometry::Rect;
use crate::geotiff::ground_to_projected;
use crate::map::Map;
use crate::stats::coordinate_bounds;

/// One flight of one region's orthophotos, as its WMS serves it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Service {
    /// The region the service covers: "Emilia-Romagna".
    pub region: &'static str,
    /// The year the photos were taken.
    pub year: u16,
    /// The WMS endpoint, up to and including its `?`.
    pub url: &'static str,
    /// The layer to ask for.
    pub layer: &'static str,
    /// The photos' own ground resolution, in meters per pixel.
    pub ground_resolution: f64,
    /// The largest image, in pixels a side, to ask for in one request.
    pub max_tile_size: u32,
    /// Who to credit for the photos.
    pub attribution: &'static str,
}

/// Emilia-Romagna's AGEA orthophotos, newest first.
///
/// Neither declares a `MaxWidth`/`MaxHeight`; 2000 pixels is a size the
/// server was seen to serve in two seconds.
pub const EMILIA_ROMAGNA: &[Service] = &[
    Service {
        region: "Emilia-Romagna",
        year: 2023,
        url: "https://servizigis.regione.emilia-romagna.it/wms/agea2023_rgb?",
        layer: "Agea2023_RGB",
        ground_resolution: 0.2,
        max_tile_size: 2000,
        attribution: "AGEA 2023, Regione Emilia-Romagna",
    },
    Service {
        region: "Emilia-Romagna",
        year: 2020,
        url: "https://servizigis.regione.emilia-romagna.it/wms/agea2020_rgb?",
        layer: "Agea2020_RGB",
        ground_resolution: 0.2,
        max_tile_size: 2000,
        attribution: "AGEA 2020, Regione Emilia-Romagna",
    },
];

/// A box in a projected CRS, in its own units (meters, for every CRS this is
/// used with).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectedBox {
    /// The western edge (smallest easting).
    pub min_x: f64,
    /// The southern edge (smallest northing).
    pub min_y: f64,
    /// The eastern edge (largest easting).
    pub max_x: f64,
    /// The northern edge (largest northing).
    pub max_y: f64,
}

impl ProjectedBox {
    /// East-west extent.
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    /// North-south extent.
    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }
}

/// The box `map`'s objects cover, grown by `margin` meters on the ground on
/// every side, in the map's projected CRS (its [`epsg`] code).
///
/// An error where the map is not placed on the earth (no georeferencing, or
/// no CRS this crate can name by EPSG code) or has nothing on it.
///
/// [`epsg`]: crate::map::Georeferencing::epsg
pub fn map_footprint(map: &Map, margin: f64) -> Result<ProjectedBox, String> {
    let georef = map
        .georeferencing
        .as_ref()
        .filter(|g| g.epsg != 0)
        .ok_or("the map is not georeferenced in a projected CRS with an EPSG code")?;
    let bounds = coordinate_bounds(map).ok_or("the map has no objects")?;
    let scale = if georef.scale > 0 {
        georef.scale
    } else {
        map.scale_denominator
    };
    let meters_per_mm = f64::from(scale) / 1000.0;
    let ground = Rect::from_ltrb(
        bounds.left() * meters_per_mm - margin,
        bounds.top() * meters_per_mm - margin,
        bounds.right() * meters_per_mm + margin,
        bounds.bottom() * meters_per_mm + margin,
    );
    let corners = [
        (ground.left(), ground.top()),
        (ground.right(), ground.top()),
        (ground.right(), ground.bottom()),
        (ground.left(), ground.bottom()),
    ];
    let mut footprint = ProjectedBox {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    for (x, y) in corners {
        let p = ground_to_projected(georef, geo::Coord { x, y })
            .ok_or("the map is not georeferenced")?;
        footprint.min_x = footprint.min_x.min(p.x);
        footprint.min_y = footprint.min_y.min(p.y);
        footprint.max_x = footprint.max_x.max(p.x);
        footprint.max_y = footprint.max_y.max(p.y);
    }
    Ok(footprint)
}

/// A north-up pixel grid in a projected CRS: square pixels, row 0 along the
/// northern edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// The western edge of column 0.
    pub west: f64,
    /// The northern edge of row 0.
    pub north: f64,
    /// The side of one pixel, in the CRS's units.
    pub pixel_size: f64,
    /// Columns.
    pub width: u32,
    /// Rows.
    pub height: u32,
}

/// One piece of a [`Grid`], in its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    /// The tile's first column.
    pub x: u32,
    /// The tile's first row.
    pub y: u32,
    /// Columns.
    pub width: u32,
    /// Rows.
    pub height: u32,
}

impl Grid {
    /// The smallest grid of `pixel_size` pixels covering `area`, its edges on
    /// whole multiples of `pixel_size`, so two grids of the same pixel size
    /// line up pixel for pixel.
    pub fn covering(area: &ProjectedBox, pixel_size: f64) -> Grid {
        let west = (area.min_x / pixel_size).floor() * pixel_size;
        let east = (area.max_x / pixel_size).ceil() * pixel_size;
        let south = (area.min_y / pixel_size).floor() * pixel_size;
        let north = (area.max_y / pixel_size).ceil() * pixel_size;
        Grid {
            west,
            north,
            pixel_size,
            width: ((east - west) / pixel_size).round().max(1.0) as u32,
            height: ((north - south) / pixel_size).round().max(1.0) as u32,
        }
    }

    /// How many pixels the whole grid has.
    pub fn pixel_count(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// The grid cut into tiles no wider or taller than `max_size`, row by
    /// row from the north-west corner.
    pub fn tiles(&self, max_size: u32) -> Vec<Tile> {
        let max_size = max_size.max(1);
        let mut tiles = Vec::new();
        for y in (0..self.height).step_by(max_size as usize) {
            for x in (0..self.width).step_by(max_size as usize) {
                tiles.push(Tile {
                    x,
                    y,
                    width: max_size.min(self.width - x),
                    height: max_size.min(self.height - y),
                });
            }
        }
        tiles
    }

    /// The ground a tile of this grid covers.
    pub fn tile_box(&self, tile: &Tile) -> ProjectedBox {
        let px = self.pixel_size;
        ProjectedBox {
            min_x: self.west + f64::from(tile.x) * px,
            max_x: self.west + f64::from(tile.x + tile.width) * px,
            max_y: self.north - f64::from(tile.y) * px,
            min_y: self.north - f64::from(tile.y + tile.height) * px,
        }
    }

    /// The ground the whole grid covers.
    pub fn bounds(&self) -> ProjectedBox {
        self.tile_box(&Tile {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        })
    }

    /// The world file placing an image of this grid on the earth: six lines,
    /// the pixel size along x, two rotation terms (0 here), the pixel size
    /// along y (negative, rows go south), and the *centre* of the top-left
    /// pixel.
    pub fn world_file(&self) -> String {
        let px = self.pixel_size;
        format!(
            "{px}\n0\n0\n{}\n{}\n{}\n",
            -px,
            self.west + px / 2.0,
            self.north - px / 2.0
        )
    }
}

/// The WMS 1.1.1 `GetMap` request for `area` (in EPSG `epsg`) from
/// `service`, as a `width` by `height` image of type `format` (a MIME type:
/// `image/jpeg`, `image/png`). With `transparent`, what the service has no
/// photo for comes back transparent, which is how coverage is told apart from
/// a dark photo.
pub fn get_map_url(
    service: &Service,
    epsg: i32,
    area: &ProjectedBox,
    width: u32,
    height: u32,
    format: &str,
    transparent: bool,
) -> String {
    format!(
        "{}SERVICE=WMS&VERSION=1.1.1&REQUEST=GetMap&LAYERS={}&STYLES=&SRS=EPSG:{epsg}\
         &BBOX={},{},{},{}&WIDTH={width}&HEIGHT={height}&FORMAT={}&TRANSPARENT={}",
        service.url,
        service.layer,
        area.min_x,
        area.min_y,
        area.max_x,
        area.max_y,
        format.replace('/', "%2F"),
        if transparent { "TRUE" } else { "FALSE" },
    )
}

/// The WMS 1.1.1 `GetCapabilities` request for `service`.
pub fn capabilities_url(service: &Service) -> String {
    format!(
        "{}SERVICE=WMS&VERSION=1.1.1&REQUEST=GetCapabilities",
        service.url
    )
}

/// What a WMS `GetCapabilities` document says it serves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capabilities {
    /// Every layer's `<Name>`.
    pub layers: Vec<String>,
    /// Every CRS any layer is offered in: `<SRS>` (1.1.1) or `<CRS>`
    /// (1.3.0), as written ("EPSG:32632").
    pub crs: Vec<String>,
    /// The first `<LatLonBoundingBox>` (1.1.1): the area the service
    /// covers, at most, in degrees. `None` where there is none.
    pub lat_lon_box: Option<LatLonBox>,
}

/// An area in WGS84 degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatLonBox {
    /// Westernmost longitude.
    pub min_lon: f64,
    /// Southernmost latitude.
    pub min_lat: f64,
    /// Easternmost longitude.
    pub max_lon: f64,
    /// Northernmost latitude.
    pub max_lat: f64,
}

impl LatLonBox {
    /// Whether the point `lat`, `lon` is inside, edges included.
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        (self.min_lat..=self.max_lat).contains(&lat) && (self.min_lon..=self.max_lon).contains(&lon)
    }
}

/// Where a Mapper map file says its reference point is, in WGS84 degrees:
/// `(latitude, longitude)` from `<geographic_crs><ref_point_deg>`. Mapper
/// keeps it beside the projected one whatever the map's CRS, so it says
/// roughly where on the earth a map is even in a CRS nothing here can
/// convert from. `None` where the file has none.
pub fn geographic_ref_point(xml: &str) -> Option<(f64, f64)> {
    let at = xml.find("<ref_point_deg ")?;
    let tag = &xml[at..at + xml[at..].find('>')?];
    let value = |name: &str| -> Option<f64> {
        let key = format!(" {name}=\"");
        let start = tag.find(&key)? + key.len();
        tag[start..start + tag[start..].find('"')?].parse().ok()
    };
    Some((value("lat")?, value("lon")?))
}

impl Capabilities {
    /// Reads a `GetCapabilities` response.
    pub fn parse(xml: &str) -> Result<Capabilities, String> {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut capabilities = Capabilities::default();
        let mut path: Vec<String> = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                }
                Ok(Event::Empty(e))
                    if e.local_name().as_ref() == b"LatLonBoundingBox"
                        && capabilities.lat_lon_box.is_none() =>
                {
                    let value = |name: &[u8]| -> Option<f64> {
                        let attr = e.try_get_attribute(name).ok()??;
                        std::str::from_utf8(&attr.value).ok()?.trim().parse().ok()
                    };
                    if let (Some(min_lon), Some(min_lat), Some(max_lon), Some(max_lat)) = (
                        value(b"minx"),
                        value(b"miny"),
                        value(b"maxx"),
                        value(b"maxy"),
                    ) {
                        capabilities.lat_lon_box = Some(LatLonBox {
                            min_lon,
                            min_lat,
                            max_lon,
                            max_lat,
                        });
                    }
                }
                Ok(Event::End(_)) => {
                    path.pop();
                }
                Ok(Event::Text(text)) => {
                    let text = text.unescape().map_err(|e| e.to_string())?;
                    let text = text.trim();
                    let n = path.len();
                    let in_layer = n >= 2 && path[n - 2] == "Layer";
                    match path.last().map(String::as_str) {
                        Some("Name") if in_layer => capabilities.layers.push(text.to_owned()),
                        Some("SRS" | "CRS") => {
                            // 1.1.1 allows several, space-separated, in one.
                            for crs in text.split_whitespace() {
                                if !capabilities.crs.iter().any(|c| c == crs) {
                                    capabilities.crs.push(crs.to_owned());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => return Err(format!("not a WMS capabilities document: {e}")),
            }
        }
        if capabilities.layers.is_empty() {
            return Err("not a WMS capabilities document: it names no layer".to_owned());
        }
        Ok(capabilities)
    }

    /// Whether EPSG `epsg` is one of [`crs`](Self::crs).
    pub fn offers_epsg(&self, epsg: i32) -> bool {
        let wanted = format!("EPSG:{epsg}");
        self.crs.iter().any(|c| c.eq_ignore_ascii_case(&wanted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{Coord, Georeferencing, Object, ObjectKind};

    fn map_with(georef: Georeferencing, coords: &[(f64, f64)]) -> Map {
        let mut map = Map::new();
        map.scale_denominator = georef.scale;
        let mut object = Object::new(ObjectKind::Point);
        object.coords = coords.iter().map(|&(x, y)| Coord::new(x, y, 0)).collect();
        map.objects.push(object);
        map.georeferencing = Some(georef);
        map
    }

    fn utm32(scale: i32, grivation: f64) -> Georeferencing {
        Georeferencing {
            scale,
            epsg: 32632,
            ref_point_x: 686_000.0,
            ref_point_y: 4_929_000.0,
            grivation,
            ..Default::default()
        }
    }

    /// Unrotated, a millimetre of paper at 1:10000 is ten meters, paper `y`
    /// grows southwards, and the margin goes on every side.
    #[test]
    fn the_footprint_is_the_objects_on_the_ground_plus_the_margin() {
        let map = map_with(utm32(10_000, 0.0), &[(0.0, 0.0), (20.0, 10.0)]);
        let footprint = map_footprint(&map, 5.0).unwrap();
        assert_eq!(
            footprint,
            ProjectedBox {
                min_x: 686_000.0 - 5.0,
                max_x: 686_200.0 + 5.0,
                min_y: 4_929_000.0 - 100.0 - 5.0,
                max_y: 4_929_000.0 + 5.0,
            }
        );
    }

    /// A map turned by its grivation needs a larger box around it: a square
    /// turned by 45 degrees needs one √2 times as wide.
    #[test]
    fn a_rotated_map_gets_the_box_around_it() {
        let map = map_with(utm32(10_000, 45.0), &[(-10.0, -10.0), (10.0, 10.0)]);
        let footprint = map_footprint(&map, 0.0).unwrap();
        let side = 200.0 * 2f64.sqrt();
        assert!((footprint.width() - side).abs() < 1e-6, "{footprint:?}");
        assert!((footprint.height() - side).abs() < 1e-6, "{footprint:?}");
        assert!((footprint.min_x + side / 2.0 - 686_000.0).abs() < 1e-6);
    }

    #[test]
    fn a_map_not_on_the_earth_has_no_footprint() {
        let local = Georeferencing {
            scale: 10_000,
            ..Default::default()
        };
        assert!(map_footprint(&map_with(local, &[(0.0, 0.0)]), 0.0).is_err());
        let mut empty = map_with(utm32(10_000, 0.0), &[]);
        empty.objects.clear();
        assert!(map_footprint(&empty, 0.0).is_err());
    }

    /// The grid snaps outwards to whole pixels and its tiles cover it once,
    /// the last ones cut short.
    #[test]
    fn the_grid_covers_the_box_in_whole_tiles() {
        let area = ProjectedBox {
            min_x: 100.03,
            min_y: 200.01,
            max_x: 500.0,
            max_y: 350.19,
        };
        let grid = Grid::covering(&area, 0.2);
        assert!((grid.west - 100.0).abs() < 1e-9 && (grid.north - 350.2).abs() < 1e-9);
        assert_eq!((grid.width, grid.height), (2000, 751));
        let bounds = grid.bounds();
        assert!(bounds.min_x <= area.min_x && bounds.max_x >= area.max_x);
        assert!(bounds.min_y <= area.min_y && bounds.max_y >= area.max_y);

        let tiles = grid.tiles(600);
        assert_eq!(tiles.len(), 4 * 2);
        let covered: u64 = tiles
            .iter()
            .map(|t| u64::from(t.width) * u64::from(t.height))
            .sum();
        assert_eq!(covered, grid.pixel_count());
        assert_eq!(tiles.last().unwrap().width, 200);
        assert_eq!(tiles.last().unwrap().height, 151);

        let last = grid.tile_box(tiles.last().unwrap());
        assert!((last.max_x - bounds.max_x).abs() < 1e-9);
        assert!((last.min_y - bounds.min_y).abs() < 1e-9);
    }

    /// The world file names the centre of the top-left pixel, not its corner.
    #[test]
    fn the_world_file_points_at_the_first_pixels_centre() {
        let grid = Grid {
            west: 1000.0,
            north: 2000.0,
            pixel_size: 0.5,
            width: 10,
            height: 10,
        };
        assert_eq!(grid.world_file(), "0.5\n0\n0\n-0.5\n1000.25\n1999.75\n");
    }

    #[test]
    fn the_get_map_request_is_easting_first() {
        let service = &EMILIA_ROMAGNA[0];
        let area = ProjectedBox {
            min_x: 686400.0,
            min_y: 4928800.0,
            max_x: 686800.0,
            max_y: 4929200.0,
        };
        let url = get_map_url(service, 32632, &area, 2000, 2000, "image/jpeg", false);
        assert!(url.starts_with(service.url));
        assert!(url.contains("&SRS=EPSG:32632&BBOX=686400,4928800,686800,4929200&"));
        assert!(url.contains("&LAYERS=Agea2023_RGB&"));
        assert!(url.contains("&FORMAT=image%2Fjpeg&TRANSPARENT=FALSE"));
    }

    #[test]
    fn reads_where_a_map_says_it_is_in_degrees() {
        let xml = r#"<georeferencing scale="15000"><projected_crs id="EPSG"><parameter>3006</parameter></projected_crs><geographic_crs id="Geographic coordinates"><spec language="PROJ.4">+proj=latlong +datum=WGS84</spec><ref_point_deg lat="57.68464003" lon="12.02263524"/></geographic_crs></georeferencing>"#;
        assert_eq!(geographic_ref_point(xml), Some((57.68464003, 12.02263524)));
        assert_eq!(geographic_ref_point("<map/>"), None);
    }

    /// The parts of a real (trimmed) capabilities document that matter.
    #[test]
    fn reads_the_layers_and_crs_a_server_offers() {
        let xml = r#"<?xml version="1.0"?>
<WMT_MS_Capabilities version="1.1.1"><Service><Name>OGC:WMS</Name></Service>
<Capability><Layer><Name>root</Name><SRS>EPSG:32632 EPSG:3003</SRS><SRS>EPSG:25832</SRS>
<LatLonBoundingBox minx="9.162086" miny="43.700591" maxx="12.868449" maxy="45.154494"/>
<Layer><Name>Agea2023_RGB</Name><Title>Agea2023_RGB</Title><SRS>EPSG:32632</SRS></Layer>
</Layer></Capability></WMT_MS_Capabilities>"#;
        let capabilities = Capabilities::parse(xml).unwrap();
        assert_eq!(capabilities.layers, ["root", "Agea2023_RGB"]);
        assert_eq!(capabilities.crs, ["EPSG:32632", "EPSG:3003", "EPSG:25832"]);
        assert!(capabilities.offers_epsg(25832));
        let area = capabilities.lat_lon_box.unwrap();
        assert_eq!((area.min_lon, area.max_lat), (9.162086, 45.154494));
        assert!(area.contains(44.4949, 11.3426)); // Bologna
        assert!(!area.contains(57.6846, 12.0226)); // Gothenburg
        assert!(!capabilities.offers_epsg(32631));
        assert!(Capabilities::parse("<html><body>401</body></html>").is_err());
    }
}
