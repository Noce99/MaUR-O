//! What to ask an orthophoto server for, so a map can be drawn over the
//! aerial photo of its own ground.
//!
//! Aerial photos are flown and published country by country, and often
//! region by region: Italy's regions and Germany's Länder each serve their
//! own. A [`Service`] is one such WMS layer, and
//! [`SERVICES`](crate::orthophoto_services::SERVICES) lists every one this
//! crate knows of, best first wherever two cover the same ground.
//!
//! Nothing here touches the network: this module says which URLs to fetch and
//! where the result lands, and the `add_orthophoto` tool does the fetching.
//! The steps are:
//!
//! 1. [`geographic_anchor`]: where on the earth the map is, in degrees,
//!    which picks the services worth asking ([`Service::covers`]).
//! 2. [`request_crs`]: the CRS to ask in. The map's own where the service
//!    offers it, so the photo needs no reprojecting; otherwise Web Mercator
//!    (EPSG:3857), or plain latitude and longitude (EPSG:4326), which
//!    Mapper reprojects onto the map itself.
//! 3. [`footprint`]: the box a map's objects cover, plus a margin, in that
//!    CRS. It is the axis-aligned box *around* the map, since a map turned
//!    by its grivation is not axis-aligned itself.
//! 4. [`Grid::covering`]: that box as a pixel grid, its edges on whole
//!    multiples of the pixel size ([`pixel_size`]).
//! 5. [`Grid::tiles`] and [`get_map_url`]: the grid in pieces no larger than
//!    a server hands out in one request, and the `GetMap` request for each.
//! 6. [`Grid::world_file`]: where the stitched image is on the earth, as the
//!    world file Mapper reads beside it (`src/templates/world_file.cpp`).
//!
//! Requests use WMS 1.1.1, whose `SRS` bounding box is always easting (or
//! longitude) first. In 1.3.0 the axis order is whatever the EPSG registry
//! says for that CRS, and many (EPSG:4326 itself, RDN2008's "N-E" UTM,
//! EPSG:7791) are northing first.

use crate::geometry::Rect;
use crate::geotiff::ground_to_projected;
use crate::map::{Georeferencing, Map};
use crate::osm_convert::utm;
use crate::stats::coordinate_bounds;

/// Web Mercator: what nearly every WMS offers, whatever else it does.
pub const WEB_MERCATOR: i32 = 3857;

/// WGS84 latitude and longitude, in degrees.
pub const WGS84: i32 = 4326;

/// The WGS 84 ellipsoid's semi-major axis, in meters.
const WGS84_A: f64 = 6_378_137.0;

/// The WGS 84 ellipsoid's first eccentricity, squared.
const WGS84_E2: f64 = 0.006_694_379_990_141_32;

/// One orthophoto layer of one WMS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Service {
    /// A short name to pick it by: `it-emilia-romagna-2023`.
    pub id: &'static str,
    /// The ground it covers, for people: "Emilia-Romagna, Italy".
    pub area: &'static str,
    /// When the photos were taken: "2023", or "2021–2023" for a mosaic.
    pub years: &'static str,
    /// The WMS endpoint, up to and including its `?` (or a trailing `&`).
    pub url: &'static str,
    /// The WMS version to speak: `1.1.1` wherever the server does, since
    /// its bounding boxes are always easting first, or `1.3.0` for a
    /// server that speaks nothing else (see [`get_map_url`]).
    pub version: &'static str,
    /// The layer to ask for.
    pub layer: &'static str,
    /// The `STYLES` to ask for: empty for the default, as most servers want.
    pub styles: &'static str,
    /// The CRSs (EPSG codes) the service was seen to serve the layer in.
    /// Asked for even where its capabilities do not list them, or cannot
    /// be read: some servers' capabilities are truncated or malformed, and
    /// some serve more than they list.
    pub crs: &'static [i32],
    /// The photos' own ground resolution, in meters per pixel.
    pub ground_resolution: f64,
    /// The largest image, in pixels a side, to ask for in one request.
    pub max_tile_size: u32,
    /// Roughly where it has photos: a box around the region. Where the box
    /// is larger than the region, a coverage probe ([`is_blank`]) tells the
    /// rest apart.
    pub coverage: LatLonBox,
    /// Who to credit for the photos.
    pub attribution: &'static str,
    /// The terms they are published under: "CC BY 4.0".
    pub licence: &'static str,
}

impl Service {
    /// Whether `lat`, `lon` is inside the service's [`coverage`](Self::coverage)
    /// box: worth asking, not yet a promise of photos.
    pub fn covers(&self, lat: f64, lon: f64) -> bool {
        self.coverage.contains(lat, lon)
    }
}

/// Whether an RGBA pixel of a coverage probe (a `TRANSPARENT=TRUE` PNG) is
/// "no photo here".
///
/// WMS means that to be transparent, and most services draw it so. Some
/// ignore `TRANSPARENT` and draw it opaque white, some black, and one
/// (Lazio's) either, depending on the size asked for. A real photo is never
/// exactly black, and is near-white only where something bright fills it,
/// so all three count.
pub fn is_blank([r, g, b, a]: [u8; 4]) -> bool {
    a == 0 || (r == 0 && g == 0 && b == 0) || (r >= 250 && g >= 250 && b >= 250)
}

/// A box in a CRS, in its own units: meters for a projected one, degrees
/// (longitude as `x`) for EPSG:4326.
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

    /// The smallest box around `points`.
    fn around(points: impl IntoIterator<Item = (f64, f64)>) -> ProjectedBox {
        let mut area = ProjectedBox {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        for (x, y) in points {
            area.min_x = area.min_x.min(x);
            area.min_y = area.min_y.min(y);
            area.max_x = area.max_x.max(x);
            area.max_y = area.max_y.max(y);
        }
        area
    }
}

/// Where a map is on the earth, as its file says beside its projected CRS:
/// Mapper keeps both, whatever the CRS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeographicAnchor {
    /// The reference point's latitude, in degrees:
    /// `<geographic_crs><ref_point_deg lat>`.
    pub lat: f64,
    /// Its longitude, in degrees.
    pub lon: f64,
    /// The angle from true north to the map's north, in degrees, as
    /// `<georeferencing declination>` says (0 where it says nothing, as
    /// Mapper reads it).
    pub declination: f64,
}

/// Where a Mapper map file says it is ([`GeographicAnchor`]), read from its
/// text. `None` where it has no `<ref_point_deg>`: a map in local
/// coordinates, not placed on the earth.
pub fn geographic_anchor(xml: &str) -> Option<GeographicAnchor> {
    let tag_at = |name: &str| -> Option<&str> {
        let at = xml.find(&format!("<{name} "))?;
        Some(&xml[at..at + xml[at..].find('>')?])
    };
    let value = |tag: &str, name: &str| -> Option<f64> {
        let key = format!(" {name}=\"");
        let start = tag.find(&key)? + key.len();
        tag[start..start + tag[start..].find('"')?].parse().ok()
    };
    let point = tag_at("ref_point_deg")?;
    let declination = tag_at("georeferencing")
        .and_then(|tag| value(tag, "declination"))
        .unwrap_or(0.0);
    Some(GeographicAnchor {
        lat: value(point, "lat")?,
        lon: value(point, "lon")?,
        declination,
    })
}

/// The CRS to ask a service in, for a map in EPSG `map_epsg` (0 where it
/// has no EPSG code), given whether the service `offers` a CRS: the map's
/// own if offered, else [`WEB_MERCATOR`], else [`WGS84`]. `None` where it
/// offers none of them.
pub fn request_crs(offers: impl Fn(i32) -> bool, map_epsg: i32) -> Option<i32> {
    [map_epsg, WEB_MERCATOR, WGS84]
        .into_iter()
        .find(|&epsg| epsg != 0 && offers(epsg))
}

/// The map's objects on the ground, unrotated, in the meters
/// [`ground_to_projected`] takes, grown by `margin` on every side.
fn ground_bounds(map: &Map, margin: f64) -> Result<(Rect, &Georeferencing), String> {
    let georef = map
        .georeferencing
        .as_ref()
        .ok_or("the map is not georeferenced")?;
    let bounds = coordinate_bounds(map).ok_or("the map has no objects")?;
    let meters_per_mm = f64::from(map_scale(map, georef)) / 1000.0;
    Ok((
        Rect::from_ltrb(
            bounds.left() * meters_per_mm - margin,
            bounds.top() * meters_per_mm - margin,
            bounds.right() * meters_per_mm + margin,
            bounds.bottom() * meters_per_mm + margin,
        ),
        georef,
    ))
}

fn map_scale(map: &Map, georef: &Georeferencing) -> i32 {
    if georef.scale > 0 {
        georef.scale
    } else {
        map.scale_denominator
    }
}

/// The points around a ground box worth projecting to find the box around
/// it in another CRS: its corners and the middles of its edges.
fn outline(ground: &Rect) -> [(f64, f64); 8] {
    let (l, t, r, b) = (ground.left(), ground.top(), ground.right(), ground.bottom());
    let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
    [
        (l, t),
        (cx, t),
        (r, t),
        (r, cy),
        (r, b),
        (cx, b),
        (l, b),
        (l, cy),
    ]
}

/// The box `map`'s objects cover, grown by `margin` meters on the ground on
/// every side, in the map's own projected CRS (its [`epsg`] code).
///
/// An error where the map is not placed on the earth (no georeferencing, or
/// no CRS this crate can name by EPSG code) or has nothing on it.
///
/// [`epsg`]: crate::map::Georeferencing::epsg
pub fn map_footprint(map: &Map, margin: f64) -> Result<ProjectedBox, String> {
    let (ground, georef) = ground_bounds(map, margin)?;
    if georef.epsg == 0 {
        return Err("the map is not georeferenced in a projected CRS with an EPSG code".into());
    }
    let corners = outline(&ground).map(|(x, y)| {
        let p = ground_to_projected(georef, geo::Coord { x, y }).expect("the map has an EPSG code");
        (p.x, p.y)
    });
    Ok(ProjectedBox::around(corners))
}

/// The box `map`'s objects cover, grown by `margin` meters, in EPSG `epsg`:
/// [`map_footprint`] where that is the map's own CRS, otherwise
/// [`WEB_MERCATOR`] or [`WGS84`] by way of the map's [`GeographicAnchor`].
///
/// In another CRS the box is approximate: the ground is laid out from the
/// anchor on the ellipsoid, turned to true north, which is good to a few
/// meters in ten kilometers. Half a percent of the map's size goes on the
/// margin to make up for it. Where the image lands is not approximate: its
/// world file is exact in its own CRS, and Mapper reprojects it.
pub fn footprint(
    map: &Map,
    anchor: &GeographicAnchor,
    epsg: i32,
    margin: f64,
) -> Result<ProjectedBox, String> {
    let (ground, georef) = ground_bounds(map, margin)?;
    if epsg != 0 && epsg == georef.epsg {
        return map_footprint(map, margin);
    }
    if epsg != WEB_MERCATOR && epsg != WGS84 {
        return Err(format!(
            "cannot work out the map's footprint in EPSG:{epsg}"
        ));
    }
    let slack = 5.0 + 0.005 * ground.width().max(ground.height());
    let ground = Rect::from_ltrb(
        ground.left() - slack,
        ground.top() - slack,
        ground.right() + slack,
        ground.bottom() + slack,
    );
    let north = true_north_rotation(georef, anchor);
    let points = outline(&ground).map(|(x, y)| {
        let (lat, lon) = ground_to_geographic(georef, anchor, north, (x, y));
        if epsg == WGS84 {
            (lon, lat)
        } else {
            web_mercator(lat, lon)
        }
    });
    Ok(ProjectedBox::around(points))
}

/// The angle from true north to map north, in degrees: the file's own
/// declination, or, for a map in UTM, its grivation plus the grid's
/// convergence there, worked out exactly.
///
/// Mapper places a map on the earth by its projected CRS and grivation and
/// only keeps the declination beside them, so where the CRS is one this
/// crate can project, that is what to trust.
fn true_north_rotation(georef: &Georeferencing, anchor: &GeographicAnchor) -> f64 {
    let zone = match georef.epsg {
        32601..=32660 => georef.epsg - 32600,
        25828..=25838 => georef.epsg - 25800,
        _ => return anchor.declination,
    };
    let step = 1e-4;
    let (e0, n0) = utm(zone as u32, (anchor.lon, anchor.lat));
    let (e1, n1) = utm(zone as u32, (anchor.lon, anchor.lat + step));
    // True north in the grid, east of grid north: minus the convergence.
    let true_north_in_grid = (e1 - e0).atan2(n1 - n0).to_degrees();
    georef.grivation - true_north_in_grid
}

/// A point on the ground (unrotated map meters, as [`ground_to_projected`]
/// takes them) to `(latitude, longitude)` in degrees, laid out from the
/// anchor on the ellipsoid, `north` degrees between true and map north.
fn ground_to_geographic(
    georef: &Georeferencing,
    anchor: &GeographicAnchor,
    north: f64,
    (x, y): (f64, f64),
) -> (f64, f64) {
    let meters_per_mm = f64::from(georef.scale.max(1)) / 1000.0;
    let (lx, ly) = (
        x - georef.map_ref_point_x * meters_per_mm,
        y - georef.map_ref_point_y * meters_per_mm,
    );
    let (sin_d, cos_d) = north.to_radians().sin_cos();
    let east = lx * cos_d - ly * sin_d;
    let north = -(lx * sin_d + ly * cos_d);

    let phi0 = anchor.lat.to_radians();
    let first = phi0 + north / meridian_radius(phi0);
    let phi = phi0 + north / meridian_radius((phi0 + first) / 2.0);
    let middle = (phi0 + phi) / 2.0;
    let lambda = anchor.lon.to_radians() + east / (prime_vertical_radius(middle) * middle.cos());
    (phi.to_degrees(), lambda.to_degrees())
}

/// The ellipsoid's radius of curvature along a meridian at latitude `phi`
/// (radians): meters of ground per radian of latitude.
fn meridian_radius(phi: f64) -> f64 {
    WGS84_A * (1.0 - WGS84_E2) / (1.0 - WGS84_E2 * phi.sin().powi(2)).powf(1.5)
}

/// The ellipsoid's radius of curvature across a meridian at latitude `phi`:
/// times `cos(phi)`, meters of ground per radian of longitude.
fn prime_vertical_radius(phi: f64) -> f64 {
    WGS84_A / (1.0 - WGS84_E2 * phi.sin().powi(2)).sqrt()
}

/// `(lat, lon)` in degrees to Web Mercator `(x, y)` in its meters.
pub fn web_mercator(lat: f64, lon: f64) -> (f64, f64) {
    let phi = lat.to_radians();
    (
        WGS84_A * lon.to_radians(),
        WGS84_A * (std::f64::consts::FRAC_PI_4 + phi / 2.0).tan().ln(),
    )
}

/// A pixel `ground` meters on a side, at latitude `lat`, as `(width,
/// height)` in EPSG `epsg`'s units: the same in the map's own projected CRS
/// (`map_epsg`), stretched by Web Mercator's scale there, or in degrees.
pub fn pixel_size(epsg: i32, map_epsg: i32, lat: f64, ground: f64) -> (f64, f64) {
    let phi = lat.to_radians();
    match epsg {
        WEB_MERCATOR if map_epsg != WEB_MERCATOR => {
            let side = ground / phi.cos();
            (side, side)
        }
        WGS84 if map_epsg != WGS84 => (
            (ground / (prime_vertical_radius(phi) * phi.cos())).to_degrees(),
            (ground / meridian_radius(phi)).to_degrees(),
        ),
        _ => (ground, ground),
    }
}

/// A north-up pixel grid in a CRS: row 0 along the northern edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// The western edge of column 0.
    pub west: f64,
    /// The northern edge of row 0.
    pub north: f64,
    /// The width of one pixel, in the CRS's units.
    pub pixel_width: f64,
    /// The height of one pixel, in the CRS's units.
    pub pixel_height: f64,
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
    /// The smallest grid of `pixel_width` by `pixel_height` pixels covering
    /// `area`, its edges on whole multiples of the pixel size, so two grids
    /// of the same pixel size line up pixel for pixel.
    pub fn covering(area: &ProjectedBox, (pixel_width, pixel_height): (f64, f64)) -> Grid {
        let west = (area.min_x / pixel_width).floor() * pixel_width;
        let east = (area.max_x / pixel_width).ceil() * pixel_width;
        let south = (area.min_y / pixel_height).floor() * pixel_height;
        let north = (area.max_y / pixel_height).ceil() * pixel_height;
        Grid {
            west,
            north,
            pixel_width,
            pixel_height,
            width: ((east - west) / pixel_width).round().max(1.0) as u32,
            height: ((north - south) / pixel_height).round().max(1.0) as u32,
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
        let (pw, ph) = (self.pixel_width, self.pixel_height);
        ProjectedBox {
            min_x: self.west + f64::from(tile.x) * pw,
            max_x: self.west + f64::from(tile.x + tile.width) * pw,
            max_y: self.north - f64::from(tile.y) * ph,
            min_y: self.north - f64::from(tile.y + tile.height) * ph,
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
    /// the pixel width, two rotation terms (0 here), the pixel height
    /// (negative, rows go south), and the *centre* of the top-left pixel.
    pub fn world_file(&self) -> String {
        let (pw, ph) = (self.pixel_width, self.pixel_height);
        format!(
            "{pw}\n0\n0\n{}\n{}\n{}\n",
            -ph,
            self.west + pw / 2.0,
            self.north - ph / 2.0
        )
    }
}

/// The WMS `GetMap` request for `area` (in EPSG `epsg`) from `service`,
/// as a `width` by `height` image of type `format` (a MIME type:
/// `image/jpeg`, `image/png`). With `transparent`, what the service has no
/// photo for comes back transparent, which is how coverage is told apart from
/// a dark photo.
///
/// In WMS 1.3.0 the CRS is `CRS`, not `SRS`, and the bounding box is in
/// the CRS's own axis order: latitude first for EPSG:4326. Every projected
/// CRS a service here is asked in is easting first either way.
pub fn get_map_url(
    service: &Service,
    epsg: i32,
    area: &ProjectedBox,
    width: u32,
    height: u32,
    format: &str,
    transparent: bool,
) -> String {
    let v13 = service.version == "1.3.0";
    let bbox = if v13 && epsg == WGS84 {
        [area.min_y, area.min_x, area.max_y, area.max_x]
    } else {
        [area.min_x, area.min_y, area.max_x, area.max_y]
    };
    format!(
        "{}SERVICE=WMS&VERSION={}&REQUEST=GetMap&LAYERS={}&STYLES={}&{}=EPSG:{epsg}\
         &BBOX={},{},{},{}&WIDTH={width}&HEIGHT={height}&FORMAT={}&TRANSPARENT={}",
        service.url,
        service.version,
        service.layer,
        service.styles,
        if v13 { "CRS" } else { "SRS" },
        bbox[0],
        bbox[1],
        bbox[2],
        bbox[3],
        format.replace('/', "%2F"),
        if transparent { "TRUE" } else { "FALSE" },
    )
}

/// The WMS `GetCapabilities` request for `service`.
pub fn capabilities_url(service: &Service) -> String {
    format!(
        "{}SERVICE=WMS&VERSION={}&REQUEST=GetCapabilities",
        service.url, service.version
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

    /// Whether the capabilities name every layer in `layers`, a
    /// comma-separated list as `GetMap` takes it.
    pub fn has_layers(&self, layers: &str) -> bool {
        layers
            .split(',')
            .all(|layer| self.layers.iter().any(|l| l == layer))
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
    use crate::orthophoto_services::SERVICES;

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

    /// `(lat, lon)` of a UTM zone 32 point, by inverting [`utm`] numerically.
    fn utm32_inverse((e, n): (f64, f64)) -> (f64, f64) {
        let (mut lat, mut lon) = (45.0, 9.0);
        for _ in 0..20 {
            let (e0, n0) = utm(32, (lon, lat));
            let d = 1e-6;
            let (e_lon, n_lon) = utm(32, (lon + d, lat));
            let (e_lat, n_lat) = utm(32, (lon, lat + d));
            let (a, b, c, dd) = (
                (e_lon - e0) / d,
                (e_lat - e0) / d,
                (n_lon - n0) / d,
                (n_lat - n0) / d,
            );
            let det = a * dd - b * c;
            let (re, rn) = (e - e0, n - n0);
            lon += (dd * re - b * rn) / det;
            lat += (a * rn - c * re) / det;
        }
        (lat, lon)
    }

    /// Laid out from its anchor on the ellipsoid, a corner of a turned map
    /// five kilometers away lands within two meters of where its UTM
    /// coordinates put it -- and the footprint's slack covers that.
    #[test]
    fn the_geographic_layout_agrees_with_utm() {
        let georef = Georeferencing {
            scale: 10_000,
            epsg: 32632,
            ref_point_x: 686_000.0,
            ref_point_y: 4_929_000.0,
            grivation: 3.0,
            ..Default::default()
        };
        let (lat, lon) = utm32_inverse((686_000.0, 4_929_000.0));
        // The declination the file would say: deliberately wrong, since in
        // UTM the convergence is worked out instead.
        let anchor = GeographicAnchor {
            lat,
            lon,
            declination: 99.0,
        };
        let north = true_north_rotation(&georef, &anchor);
        for corner in [(3000.0, -4000.0), (-3500.0, 3500.0), (4000.0, 3000.0)] {
            let exact = ground_to_projected(
                &georef,
                geo::Coord {
                    x: corner.0,
                    y: corner.1,
                },
            )
            .unwrap();
            let (plat, plon) = ground_to_geographic(&georef, &anchor, north, corner);
            let (e, n) = utm(32, (plon, plat));
            let miss = (e - exact.x).hypot(n - exact.y);
            assert!(miss < 2.0, "{corner:?} is {miss} m off");
        }
    }

    /// The map's own CRS is the one asked for wherever the service offers it.
    #[test]
    fn the_map_crs_is_asked_for_first_then_web_mercator() {
        let offers = |crs: &'static [i32]| move |epsg: i32| crs.contains(&epsg);
        assert_eq!(request_crs(offers(&[3857, 32632]), 32632), Some(32632));
        assert_eq!(
            request_crs(offers(&[3857, 32632]), 32631),
            Some(WEB_MERCATOR)
        );
        assert_eq!(request_crs(offers(&[4326]), 0), Some(WGS84));
        assert_eq!(request_crs(offers(&[3003]), 32632), None);
    }

    /// In Web Mercator, the footprint is the UTM one stretched by
    /// `1 / cos(lat)`, around the same place.
    #[test]
    fn the_web_mercator_footprint_surrounds_the_map() {
        let georef = utm32(10_000, 0.0);
        let map = map_with(georef.clone(), &[(-100.0, -100.0), (100.0, 100.0)]);
        let (lat, lon) = utm32_inverse((686_000.0, 4_929_000.0));
        let anchor = GeographicAnchor {
            lat,
            lon,
            declination: 0.0,
        };
        let area = footprint(&map, &anchor, WEB_MERCATOR, 0.0).unwrap();
        let stretch = 1.0 / lat.to_radians().cos();
        // 2 km of ground, plus the 0.5 % + 5 m slack on each side, turned by
        // the grid's convergence there (1.7 degrees: 3 % more box).
        let expected = (2000.0 + 2.0 * (5.0 + 10.0)) * stretch * 1.03;
        assert!(
            (area.width() - expected).abs() < 0.01 * expected,
            "{area:?}"
        );
        assert!(
            (area.height() - expected).abs() < 0.01 * expected,
            "{area:?}"
        );
        let (x, y) = web_mercator(lat, lon);
        assert!(area.min_x < x && x < area.max_x && area.min_y < y && y < area.max_y);

        let (w, h) = pixel_size(WEB_MERCATOR, 32632, lat, 0.2);
        assert!((w - 0.2 * stretch).abs() < 1e-9 && w == h);
        let degrees = footprint(&map, &anchor, WGS84, 0.0).unwrap();
        assert!(degrees.min_x < lon && lon < degrees.max_x);
        assert!(degrees.min_y < lat && lat < degrees.max_y);
        assert!(footprint(&map, &anchor, 3003, 0.0).is_err());
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
        let grid = Grid::covering(&area, (0.2, 0.2));
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
            pixel_width: 0.5,
            pixel_height: 0.25,
            width: 10,
            height: 10,
        };
        assert_eq!(grid.world_file(), "0.5\n0\n0\n-0.25\n1000.25\n1999.875\n");
    }

    #[test]
    fn the_get_map_request_is_easting_first() {
        let service = SERVICES
            .iter()
            .find(|s| s.id == "it-emilia-romagna-2023")
            .unwrap();
        let area = ProjectedBox {
            min_x: 686400.0,
            min_y: 4928800.0,
            max_x: 686800.0,
            max_y: 4929200.0,
        };
        let url = get_map_url(service, 32632, &area, 2000, 2000, "image/jpeg", false);
        assert!(url.starts_with(service.url));
        assert!(url.contains("&SRS=EPSG:32632&BBOX=686400,4928800,686800,4929200&"));
        assert!(url.contains("&LAYERS=Agea2023_RGB&STYLES=&"));
        assert!(url.contains("&FORMAT=image%2Fjpeg&TRANSPARENT=FALSE"));
        assert!(url.contains("&VERSION=1.1.1&"));
    }

    /// WMS 1.3.0 names the CRS `CRS` and puts latitude first in EPSG:4326.
    #[test]
    fn a_wms_1_3_0_request_follows_its_axis_order() {
        let service = SERVICES.iter().find(|s| s.version == "1.3.0").unwrap();
        let area = ProjectedBox {
            min_x: 2.0,
            min_y: 48.0,
            max_x: 2.5,
            max_y: 48.5,
        };
        let url = get_map_url(service, WGS84, &area, 10, 10, "image/png", true);
        assert!(url.contains("&VERSION=1.3.0&"));
        assert!(url.contains("&CRS=EPSG:4326&BBOX=48,2,48.5,2.5&"));
        let url = get_map_url(service, WEB_MERCATOR, &area, 10, 10, "image/png", true);
        assert!(url.contains("&CRS=EPSG:3857&BBOX=2,48,2.5,48.5&"));
    }

    #[test]
    fn reads_where_a_map_says_it_is_in_degrees() {
        let xml = r#"<georeferencing scale="15000" declination="5.5" grivation="3.1"><projected_crs id="EPSG"><parameter>3006</parameter></projected_crs><geographic_crs id="Geographic coordinates"><spec language="PROJ.4">+proj=latlong +datum=WGS84</spec><ref_point_deg lat="57.68464003" lon="12.02263524"/></geographic_crs></georeferencing>"#;
        assert_eq!(
            geographic_anchor(xml),
            Some(GeographicAnchor {
                lat: 57.68464003,
                lon: 12.02263524,
                declination: 5.5
            })
        );
        let undeclared = xml.replace(" declination=\"5.5\"", "");
        assert_eq!(geographic_anchor(&undeclared).unwrap().declination, 0.0);
        assert_eq!(geographic_anchor("<map/>"), None);
    }

    #[test]
    fn what_counts_as_no_photo() {
        assert!(is_blank([120, 80, 60, 0]));
        assert!(is_blank([0, 0, 0, 255]));
        assert!(is_blank([252, 255, 250, 255]));
        assert!(!is_blank([0, 0, 1, 255]));
        assert!(!is_blank([249, 255, 255, 255]));
        assert!(!is_blank([120, 80, 60, 255]));
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
        assert!(capabilities.has_layers("Agea2023_RGB"));
        assert!(capabilities.has_layers("root,Agea2023_RGB"));
        assert!(!capabilities.has_layers("Agea2023_RGB,Agea2020_RGB"));
        assert!(Capabilities::parse("<html><body>401</body></html>").is_err());
    }
}
