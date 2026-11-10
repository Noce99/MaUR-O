//! What to download from Lantmäteriet, Sweden's mapping agency, for a
//! benchmark of `contours_to_raster` on real terrain: the lidar point cloud
//! of a place (Laserdata Skog, COPC `.laz`, 2.5 km tiles) and the 1 m ground
//! model made from it (Markhöjdmodell, a cloud-optimized GeoTIFF, 10 km
//! tiles). See `mds/DEM-Benchmark-Plan.md`.
//!
//! Like [`crate::orthophoto`], this module never touches the network: it
//! builds the STAC search URLs, reads the answers, and picks the tiles and
//! the box to cut out of them. The `create_dem_benchmark` binary does the
//! HTTP (and the HTTP Basic authentication a personal GeoTorget account
//! gives, which is only ever sent to a `https://*.lantmateriet.se` URL --
//! see [`is_lantmateriet_url`]).
//!
//! Everything is in SWEREF99 TM (EPSG:3006), the CRS both products are
//! published in; [`sweref99tm`] and [`sweref99tm_inverse`] go between it and
//! latitude/longitude.

use crate::osm_convert::{transverse_mercator, transverse_mercator_inverse};
use crate::pullauta_import::GroundBox;

/// The STAC API search endpoint.
pub const STAC_SEARCH: &str = "https://api.lantmateriet.se/stac-hojd/v1/search";
/// The point cloud collection: Laserdata Skog, COPC.
pub const LAZ_COLLECTION: &str = "dsm-skoglig-copc";
/// The ground model collection: Markhöjdmodell, 1 m, cloud-optimized GeoTIFF.
pub const DTM_COLLECTION: &str = "dtm-cog";
/// SWEREF99 TM's EPSG code.
pub const SWEREF99_TM: u16 = 3006;

/// `(lon, lat)` in degrees to SWEREF99 TM `(easting, northing)` in meters.
pub fn sweref99tm(position: (f64, f64)) -> (f64, f64) {
    transverse_mercator(15.0, position)
}

/// SWEREF99 TM `(easting, northing)` in meters to `(lon, lat)` in degrees.
pub fn sweref99tm_inverse(position: (f64, f64)) -> (f64, f64) {
    transverse_mercator_inverse(15.0, position)
}

/// Sweden's mainland, as a coarse `(lon, lat)` outline: about 10 km off
/// in places, which only means a sample now and then lands in the sea or
/// across a border and finds no tiles, and is drawn again. Gotland and
/// Öland are left out.
pub const SWEDEN_OUTLINE: [(f64, f64); 40] = [
    (13.36, 55.34),
    (12.82, 55.40),
    (12.98, 55.62),
    (12.69, 56.05),
    (12.45, 56.30),
    (12.85, 56.65),
    (12.25, 57.10),
    (11.85, 57.70),
    (11.17, 58.93),
    (11.43, 59.10),
    (11.80, 59.85),
    (12.50, 60.20),
    (12.25, 61.00),
    (12.60, 61.60),
    (12.10, 62.30),
    (12.05, 63.20),
    (12.20, 63.90),
    (13.90, 64.50),
    (14.30, 65.10),
    (15.40, 66.10),
    (16.40, 67.00),
    (17.90, 68.00),
    (18.10, 68.50),
    (20.30, 69.06),
    (22.50, 68.45),
    (23.37, 67.21),
    (23.65, 66.39),
    (24.15, 65.83),
    (22.15, 65.58),
    (21.20, 64.75),
    (20.30, 63.80),
    (18.70, 63.30),
    (17.30, 62.40),
    (17.15, 60.67),
    (18.45, 60.34),
    (19.00, 59.50),
    (17.00, 58.75),
    (16.65, 57.75),
    (16.37, 56.66),
    (15.60, 56.15),
];

/// A small, seedable random number generator (SplitMix64): the same seed
/// draws the same benchmark, on any machine.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator starting from `seed`.
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number uniform in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A point drawn uniformly by area from inside [`SWEDEN_OUTLINE`], in
/// SWEREF99 TM: uniform in the projected plane (whose scale is within a
/// part in a thousand of true across Sweden), not in degrees, which would
/// crowd the north.
pub fn sample_point(rng: &mut Rng) -> (f64, f64) {
    let outline: Vec<(f64, f64)> = SWEDEN_OUTLINE.iter().map(|&p| sweref99tm(p)).collect();
    let (mut min, mut max) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for &(x, y) in &outline {
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
    }
    loop {
        let p = (
            min.0 + rng.next_f64() * (max.0 - min.0),
            min.1 + rng.next_f64() * (max.1 - min.1),
        );
        if point_in_polygon(p, &outline) {
            return p;
        }
    }
}

/// Even-odd point-in-polygon.
fn point_in_polygon(p: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

/// The STAC search URL for one collection's items whose footprint meets the
/// SWEREF99 TM rectangle `ground` (asked as the latitude/longitude box
/// around it, which the API takes).
pub fn search_url(collection: &str, ground: &GroundBox, limit: usize) -> String {
    let corners = [
        (ground.min_x, ground.min_y),
        (ground.max_x, ground.min_y),
        (ground.max_x, ground.max_y),
        (ground.min_x, ground.max_y),
    ]
    .map(sweref99tm_inverse);
    let west = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min);
    let east = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max);
    let south = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min);
    let north = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max);
    format!(
        "{STAC_SEARCH}?collections={collection}&bbox={west:.6},{south:.6},{east:.6},{north:.6}&limit={limit}"
    )
}

/// Whether `url` may be sent the account's credentials: `https`, on
/// `lantmateriet.se` or one of its subdomains, with no user info of its own.
pub fn is_lantmateriet_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "lantmateriet.se" || host.ends_with(".lantmateriet.se")
}

/// The value of an HTTP `Authorization` header for Basic authentication.
pub fn basic_auth(user: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
}

/// Standard base64, with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One STAC item of either collection, as far as the benchmark needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct StacItem {
    /// The item id, e.g. `19b002-63975_3175_25` or `639_31`.
    pub id: String,
    /// The collection it belongs to.
    pub collection: String,
    /// The URL of its data file (the `.laz` or the `.tif`).
    pub data_href: String,
    /// Its footprint in SWEREF99 TM: the `proj:bbox` the point cloud items
    /// carry, else the box around the latitude/longitude footprint
    /// projected (the ground model items carry no `proj:bbox`).
    pub footprint: GroundBox,
    /// When the ground was measured, `YYYY-MM-DD`: a ground model's
    /// `ursprungligtmatdatum` (its `datetime` is when it was processed),
    /// else the item's `datetime`.
    pub measured: Option<String>,
    /// How many points a point cloud holds (`pc:count`).
    pub point_count: Option<u64>,
}

impl StacItem {
    /// Points per square meter of footprint, for a point cloud.
    pub fn density(&self) -> Option<f64> {
        let area = (self.footprint.max_x - self.footprint.min_x)
            * (self.footprint.max_y - self.footprint.min_y);
        self.point_count
            .filter(|_| area > 0.0)
            .map(|n| n as f64 / area)
    }

    /// The year the ground was measured.
    pub fn year(&self) -> Option<i32> {
        self.measured.as_ref()?.get(..4)?.parse().ok()
    }
}

/// Reads a STAC search answer (a GeoJSON FeatureCollection) into its items.
/// An item without a data asset is left out.
pub fn parse_search(text: &str) -> Result<Vec<StacItem>, String> {
    let json = Json::parse(text)?;
    let features = json
        .get("features")
        .and_then(Json::as_array)
        .ok_or("the STAC answer has no \"features\"")?;
    let mut items = Vec::new();
    for feature in features {
        let Some(id) = feature.get("id").and_then(Json::as_str) else {
            continue;
        };
        let collection = feature
            .get("collection")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let Some(data_href) = feature
            .get("assets")
            .and_then(|a| a.get("data"))
            .and_then(|d| d.get("href"))
            .and_then(Json::as_str)
        else {
            continue;
        };
        let properties = feature.get("properties");
        let property = |key: &str| properties.and_then(|p| p.get(key));
        let proj_bbox = property("proj:bbox")
            .and_then(Json::as_array)
            .and_then(|a| a.iter().map(Json::as_f64).collect::<Option<Vec<f64>>>())
            .filter(|v| v.len() == 4);
        let footprint = match proj_bbox {
            Some(b) => GroundBox {
                min_x: b[0],
                min_y: b[1],
                max_x: b[2],
                max_y: b[3],
            },
            None => match geometry_footprint(feature.get("geometry")) {
                Some(f) => f,
                None => continue,
            },
        };
        let date = |key: &str| {
            property(key)
                .and_then(Json::as_str)
                .and_then(|s| s.get(..10))
                .map(str::to_string)
        };
        items.push(StacItem {
            id: id.to_string(),
            collection: collection.to_string(),
            data_href: data_href.to_string(),
            footprint,
            measured: date("ursprungligtmatdatum").or_else(|| date("datetime")),
            point_count: property("pc:count")
                .and_then(Json::as_f64)
                .map(|n| n as u64),
        });
    }
    Ok(items)
}

/// The SWEREF99 TM box around a GeoJSON polygon's outer ring, in degrees.
fn geometry_footprint(geometry: Option<&Json>) -> Option<GroundBox> {
    let ring = geometry?
        .get("coordinates")?
        .as_array()?
        .first()?
        .as_array()?;
    let mut b = GroundBox {
        min_x: f64::MAX,
        min_y: f64::MAX,
        max_x: f64::MIN,
        max_y: f64::MIN,
    };
    for point in ring {
        let p = point.as_array()?;
        let (x, y) = sweref99tm((p.first()?.as_f64()?, p.get(1)?.as_f64()?));
        b.min_x = b.min_x.min(x);
        b.min_y = b.min_y.min(y);
        b.max_x = b.max_x.max(x);
        b.max_y = b.max_y.max(y);
    }
    (b.min_x < b.max_x && b.min_y < b.max_y).then_some(b)
}

/// The `size`-meter square box, on whole meters, as close to centered on
/// `point` as it can be while staying `margin` meters inside `tile`; `None`
/// if the tile is too small for it.
pub fn box_in_tile(
    point: (f64, f64),
    size: f64,
    margin: f64,
    tile: &GroundBox,
) -> Option<GroundBox> {
    let place = |center: f64, lo: f64, hi: f64| -> Option<f64> {
        let (lo, hi) = (lo + margin, hi - margin - size);
        if hi < lo {
            return None;
        }
        Some((center - size / 2.0).round().clamp(lo.ceil(), hi.floor()))
    };
    let x = place(point.0, tile.min_x, tile.max_x)?;
    let y = place(point.1, tile.min_y, tile.max_y)?;
    Some(GroundBox {
        min_x: x,
        min_y: y,
        max_x: x + size,
        max_y: y + size,
    })
}

/// Whether two boxes share some area.
pub fn overlaps(a: &GroundBox, b: &GroundBox) -> bool {
    a.min_x < b.max_x && b.min_x < a.max_x && a.min_y < b.max_y && b.min_y < a.max_y
}

/// Whether `outer` holds all of `inner`.
pub fn covers(outer: &GroundBox, inner: &GroundBox) -> bool {
    outer.min_x <= inner.min_x
        && outer.min_y <= inner.min_y
        && outer.max_x >= inner.max_x
        && outer.max_y >= inner.max_y
}

/// A JSON value: only as much JSON as reading a STAC answer takes. Numbers
/// are `f64`; objects keep their keys in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, as key/value pairs.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Parses a whole JSON document.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = JsonParser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = p.value()?;
        p.space();
        if p.at != p.bytes.len() {
            return Err(format!("JSON: unexpected text at byte {}", p.at));
        }
        Ok(value)
    }

    /// An object's value for `key`.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The elements of an array.
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    /// A string's text.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    /// A number's value.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl JsonParser<'_> {
    fn error(&self, what: &str) -> String {
        format!("JSON: {what} at byte {}", self.at)
    }

    fn space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.at += 1;
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.bytes.get(self.at) {
            None => Err(self.error("unexpected end")),
            Some(b'{') => {
                self.at += 1;
                let mut pairs = Vec::new();
                self.space();
                if self.bytes.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Object(pairs));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.space();
                    if self.bytes.get(self.at) != Some(&b':') {
                        return Err(self.error("expected ':'"));
                    }
                    self.at += 1;
                    pairs.push((key, self.value()?));
                    self.space();
                    match self.bytes.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Object(pairs));
                        }
                        _ => return Err(self.error("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.bytes.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    match self.bytes.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Array(items));
                        }
                        _ => return Err(self.error("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(_) => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| b"+-.eE0123456789".contains(b))
                {
                    self.at += 1;
                }
                std::str::from_utf8(&self.bytes[start..self.at])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .map(Json::Number)
                    .ok_or_else(|| self.error("bad number"))
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.at) != Some(&b'"') {
            return Err(self.error("expected a string"));
        }
        self.at += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&b) = self.bytes.get(self.at) else {
                return Err(self.error("unterminated string"));
            };
            self.at += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.bytes.get(self.at) else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.at += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let mut code = self.hex4()?;
                            // A surrogate pair is one character in two escapes.
                            if (0xD800..0xDC00).contains(&code)
                                && self.bytes[self.at..].starts_with(b"\\u")
                            {
                                self.at += 2;
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            let c = char::from_u32(code).unwrap_or('\u{FFFD}');
                            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("a string is not UTF-8"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u32::from_str_radix(d, 16).ok())
            .ok_or_else(|| self.error("bad \\u escape"))?;
        self.at += 4;
        Ok(digits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed real answer of `stac-hojd/v1/search`: one ground model item
    /// and one point cloud item, as the API sent them (2026-11-10).
    const SEARCH: &str = r#"{"type":"FeatureCollection","features":[
      {"id":"639_31","bbox":[11.811349342388938,57.61227313749089,11.986352857012228,57.70607237387218],
       "type":"Feature","geometry":{"type":"Polygon","coordinates":[[[11.819214939597027,57.61227313749089],
       [11.811349342388938,57.70195977492614],[11.978898992365227,57.70607237387218],
       [11.986352857012228,57.61637159471741],[11.819214939597027,57.61227313749089]]]},
       "collection":"dtm-cog","properties":{"datetime":"2023-02-09T12:00:00Z","proj:code":"EPSG:5845",
       "ursprungligtmatdatum":"2019-11-29","hojdmodelltyp":"markhöjdmodell"},
       "assets":{"data":{"href":"https://dl1.lantmateriet.se/hojd/data/grid/mhm/63_3/m639_31.tif",
       "type":"image/tiff; application=geotiff; profile=cloud-optimized"}}},
      {"id":"19b002-63975_3175_25","type":"Feature","geometry":{"type":"Polygon","coordinates":[[
       [11.938900735508621,57.68264084406465],[11.980766603327844,57.683647356819044],
       [11.978898992365227,57.70607237387218],[11.937007325571207,57.705064993441496],
       [11.938900735508621,57.68264084406465]]]},
       "collection":"dsm-skoglig-copc","properties":{"datetime":"2019-11-29T12:00:00Z","pc:count":13598837,
       "proj:bbox":[317500.0,6397500.0,320000.0,6400000.0],"pc:density":0.6964343475},
       "assets":{"data":{"href":"https://dl1.lantmateriet.se/hojd/data/pointcloud/sls/19b002/y19b002-63975_3175_25.copc.laz",
       "type":"application/vnd.laszip+copc"}}}
    ],"numberReturned":2}"#;

    #[test]
    fn a_search_answer_reads_into_items() {
        let items = parse_search(SEARCH).unwrap();
        assert_eq!(items.len(), 2);

        let dtm = &items[0];
        assert_eq!(dtm.collection, DTM_COLLECTION);
        assert!(dtm.data_href.ends_with("m639_31.tif"));
        // When it was measured, not when it was processed.
        assert_eq!(dtm.measured.as_deref(), Some("2019-11-29"));
        // The 10 km tile from E 310 000, N 6 390 000, out of its lat/lon
        // footprint.
        let f = dtm.footprint;
        assert!((f.min_x - 310_000.0).abs() < 1.0 && (f.max_x - 320_000.0).abs() < 1.0);
        assert!((f.min_y - 6_390_000.0).abs() < 1.0 && (f.max_y - 6_400_000.0).abs() < 1.0);

        let laz = &items[1];
        assert_eq!(laz.collection, LAZ_COLLECTION);
        assert_eq!(laz.year(), Some(2019));
        assert_eq!(laz.footprint.min_x, 317_500.0);
        let density = laz.density().unwrap();
        assert!((density - 13_598_837.0 / 6_250_000.0).abs() < 1e-9);
    }

    #[test]
    fn sweref99tm_matches_lantmateriets_own_tile_corner() {
        // The point cloud tile's south-west corner, as the API gives it in
        // both CRSs.
        let (e, n) = sweref99tm((11.938900735508621, 57.68264084406465));
        assert!(
            (e - 317_500.0).abs() < 0.01 && (n - 6_397_500.0).abs() < 0.01,
            "{e} {n}"
        );
        let (lon, lat) = sweref99tm_inverse((317_500.0, 6_397_500.0));
        assert!((lon - 11.938900735508621).abs() < 1e-8 && (lat - 57.68264084406465).abs() < 1e-8);
    }

    #[test]
    fn a_search_url_asks_for_the_box_around_the_ground() {
        let ground = GroundBox {
            min_x: 317_500.0,
            min_y: 6_397_500.0,
            max_x: 320_000.0,
            max_y: 6_400_000.0,
        };
        let url = search_url(LAZ_COLLECTION, &ground, 5);
        assert!(url.starts_with(
            "https://api.lantmateriet.se/stac-hojd/v1/search?collections=dsm-skoglig-copc&bbox=11.937007,57.682641,"
        ), "{url}");
        assert!(url.ends_with("&limit=5"));
    }

    #[test]
    fn credentials_go_only_to_lantmateriet() {
        assert!(is_lantmateriet_url(
            "https://dl1.lantmateriet.se/hojd/x.tif"
        ));
        assert!(is_lantmateriet_url(
            "https://api.lantmateriet.se/stac-hojd/v1/search?x=1"
        ));
        assert!(is_lantmateriet_url("https://lantmateriet.se/"));
        assert!(!is_lantmateriet_url("http://dl1.lantmateriet.se/x.tif"));
        assert!(!is_lantmateriet_url("https://evil-lantmateriet.se/x.tif"));
        assert!(!is_lantmateriet_url(
            "https://lantmateriet.se.evil.com/x.tif"
        ));
        assert!(!is_lantmateriet_url(
            "https://user@dl1.lantmateriet.se/x.tif"
        ));
    }

    #[test]
    fn basic_auth_is_base64_of_user_colon_password() {
        assert_eq!(
            basic_auth("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn a_box_is_kept_inside_its_tile() {
        let tile = GroundBox {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 2500.0,
            max_y: 2500.0,
        };
        let b = box_in_tile((1250.0, 1250.0), 1000.0, 50.0, &tile).unwrap();
        assert_eq!(
            (b.min_x, b.min_y, b.max_x, b.max_y),
            (750.0, 750.0, 1750.0, 1750.0)
        );
        // Near a corner it is pushed back in.
        let b = box_in_tile((10.0, 2490.0), 1000.0, 50.0, &tile).unwrap();
        assert_eq!((b.min_x, b.max_y), (50.0, 2450.0));
        assert!(box_in_tile((0.0, 0.0), 2450.0, 50.0, &tile).is_none());
        assert!(covers(&tile, &b) && overlaps(&tile, &b));
    }

    #[test]
    fn samples_are_reproducible_and_inside_sweden() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..50 {
            let p = sample_point(&mut a);
            assert_eq!(p, sample_point(&mut b));
            let (lon, lat) = sweref99tm_inverse(p);
            assert!(
                (10.9..24.2).contains(&lon) && (55.3..69.1).contains(&lat),
                "{lon} {lat}"
            );
        }
    }

    #[test]
    fn json_reads_what_stac_writes() {
        let j = Json::parse(r#" {"a": [1, -2.5e1, true, null], "b": "x\"ä😀"} "#).unwrap();
        assert_eq!(
            j.get("a").unwrap().as_array().unwrap()[1].as_f64(),
            Some(-25.0)
        );
        assert_eq!(j.get("b").unwrap().as_str(), Some("x\"ä😀"));
        assert!(Json::parse("{\"a\": }").is_err());
        assert!(Json::parse("[1] x").is_err());
    }
}
