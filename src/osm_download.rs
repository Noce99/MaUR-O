//! What to ask the [Overpass API](https://wiki.openstreetmap.org/wiki/Overpass_API)
//! for the OpenStreetMap data of an area, and how to read what it says
//! back. Nothing here touches the network: `osm_to_omap --download` does the
//! asking (with `ureq`, a native-only dependency), so that this crate's wasm
//! build never sees an HTTP client.
//!
//! # The query
//!
//! [`overpass_query`] asks for every node and way in the box and the
//! multipolygons reaching into it, and then everything they are made of
//! (`>`): a multipolygon reaching out of the box comes back whole, so that
//! its rings can be closed before they are clipped — which the main OSM
//! API's `map` call, made for editors, does not do. Where asked, the bus
//! and train routes reaching into the box come back whole too, with their
//! route masters, for `--bus` and `--train`.
//!
//! The other relations are left out: an administrative boundary, a cycle
//! or hiking route passing through, downloaded whole, would be most of the
//! answer — 18 MB of 73 000 nodes for a quarter of a square kilometre of
//! Bologna — and nothing here draws them. The answer is `.osm` XML, what
//! [`crate::osm::read_osm_str`] reads.
//!
//! # Fair use
//!
//! The public Overpass servers are shared. A neighbourhood is a few
//! megabytes and seconds; a whole city is hundreds of megabytes and can
//! time out. [`MAX_AREA_KM2`] is the largest box asked for without being
//! told to; anything larger is better cut from an extract (Geofabrik's, say)
//! and converted from the file.

use crate::osm::BBox;

/// The public Overpass server asked by default.
pub const OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";

/// The largest area asked for by default, in square kilometres.
pub const MAX_AREA_KM2: f64 = 25.0;

/// How long the server may take over the query, in seconds: Overpass's own
/// `[timeout:]`, which it also uses to decide whether it has room for the
/// query at all.
pub const TIMEOUT_S: u64 = 180;

/// The `route` values of the relations `--bus` and `--train` read (see
/// [`crate::bus`]).
const TRANSIT_ROUTES: &str = "bus|trolleybus|train|light_rail|subway|tram|monorail";

/// The Overpass QL query for what is in `bbox` and everything it is made
/// of, as `.osm` XML; with `routes`, the bus and train routes reaching into
/// it too, with their route masters.
///
/// Overpass wants a box as `south,west,north,east` — latitude first —
/// where OpenStreetMap's own export and [`BBox::parse`] take
/// `min_lon,min_lat,max_lon,max_lat`.
pub fn overpass_query(bbox: &BBox, routes: bool) -> String {
    let b = format!(
        "{},{},{},{}",
        bbox.min_lat, bbox.min_lon, bbox.max_lat, bbox.max_lon
    );
    let mut query = format!(
        "[out:xml][timeout:{TIMEOUT_S}];\n\
         (node({b}); way({b}); rel({b})[type=multipolygon];)->.area;\n"
    );
    if routes {
        query += &format!(
            "rel({b})[type=route][route~\"^({TRANSIT_ROUTES})$\"]->.routes;\n\
             (.area; .area >; .routes; .routes >; rel(br.routes)[type=route_master];);\n"
        );
    } else {
        query += "(.area; .area >;);\n";
    }
    query + "out meta;\n"
}

/// The error an Overpass answer reports, if it reports one.
///
/// A query which runs out of time or memory still comes back `200 OK`,
/// with whatever was found by then and a `<remark>` saying what went wrong:
/// data that looks whole and is not.
pub fn overpass_error(answer: &str) -> Option<String> {
    let start = answer.find("<remark>")? + "<remark>".len();
    let end = start + answer[start..].find("</remark>")?;
    let remark = answer[start..end].trim();
    remark
        .to_ascii_lowercase()
        .contains("error")
        .then(|| remark.to_string())
}

/// Parses a length: `2km`, `1.5 km`, `500m` or a bare number of metres.
pub fn parse_size(text: &str) -> Result<f64, String> {
    let t = text.trim().to_ascii_lowercase();
    let (number, unit) = if let Some(n) = t.strip_suffix("km") {
        (n, 1000.0)
    } else if let Some(n) = t.strip_suffix('m') {
        (n, 1.0)
    } else {
        (t.as_str(), 1.0)
    };
    match number.trim().parse::<f64>() {
        Ok(n) if n > 0.0 && n.is_finite() => Ok(n * unit),
        _ => Err(format!(
            "not a size: {text:?} (want e.g. 2km, 500m, or metres)"
        )),
    }
}

/// Parses a point given as `lat,lon` in degrees — the order a map app copies
/// one in — into `(lon, lat)`.
pub fn parse_center(text: &str) -> Result<(f64, f64), String> {
    let bad = || format!("not a point: {text:?} (want lat,lon in degrees)");
    let values: Vec<f64> = text
        .split(',')
        .map(|v| v.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let [lat, lon] = values[..] else {
        return Err(bad());
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(bad());
    }
    Ok((lon, lat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_gives_the_box_latitude_first() {
        let bbox = BBox::parse("11.33,44.48,11.36,44.50").unwrap();
        let query = overpass_query(&bbox, false);
        assert!(query.contains("way(44.48,11.33,44.5,11.36)"), "{query}");
        assert!(query.contains("rel(44.48,11.33,44.5,11.36)[type=multipolygon]"));
        assert!(query.contains("(.area; .area >;);"));
        assert!(!query.contains("route"));
        assert!(query.starts_with("[out:xml][timeout:180];"));
        assert!(query.ends_with("out meta;\n"));
    }

    #[test]
    fn routes_are_asked_for_only_when_wanted() {
        let bbox = BBox::parse("11.33,44.48,11.36,44.50").unwrap();
        let query = overpass_query(&bbox, true);
        assert!(
            query.contains("[type=route][route~\"^(bus|trolleybus|train|"),
            "{query}"
        );
        assert!(query.contains(".routes >;"));
        assert!(query.contains("rel(br.routes)[type=route_master]"));
    }

    #[test]
    fn a_remark_with_an_error_is_an_error() {
        let timed_out = r#"<osm><remark> runtime error: Query timed out in "query" at line 2 after 181 seconds. </remark></osm>"#;
        assert!(overpass_error(timed_out)
            .unwrap()
            .starts_with("runtime error"));
        assert_eq!(overpass_error("<osm><node id=\"1\"/></osm>"), None);
        assert_eq!(overpass_error("<osm><remark>all fine</remark></osm>"), None);
    }

    #[test]
    fn sizes_are_read_in_metres() {
        assert_eq!(parse_size("2km").unwrap(), 2000.0);
        assert_eq!(parse_size("1.5 km").unwrap(), 1500.0);
        assert_eq!(parse_size("500m").unwrap(), 500.0);
        assert_eq!(parse_size("750").unwrap(), 750.0);
        assert!(parse_size("-1km").is_err());
        assert!(parse_size("big").is_err());
    }

    #[test]
    fn a_square_is_its_size_on_the_ground() {
        use crate::osm_convert::utm;
        let square = BBox::square((11.3431, 44.4939), 1000.0);
        let (x0, y0) = utm(32, (square.min_lon, square.min_lat));
        let (x1, _) = utm(32, (square.max_lon, square.min_lat));
        let (_, y1) = utm(32, (square.min_lon, square.max_lat));
        // To within the UTM grid's own scale, a few parts in ten thousand.
        assert!((x1 - x0 - 1000.0).abs() < 1.0, "{}", x1 - x0);
        assert!((y1 - y0 - 1000.0).abs() < 1.0, "{}", y1 - y0);
        assert!((square.area_km2() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn a_point_is_read_latitude_first() {
        assert_eq!(
            parse_center("44.4939, 11.3431").unwrap(),
            (11.3431, 44.4939)
        );
        assert!(parse_center("11.34").is_err());
        assert!(parse_center("95,11").is_err());
    }
}
