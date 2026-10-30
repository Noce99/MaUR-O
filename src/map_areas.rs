//! The areas of an event's maps, read from a CSV table, to be drawn over an
//! OpenStreetMap conversion: each map's outline and its name, the zones
//! forbidden to the runners, and where the start and the finish are.
//!
//! The list is a CSV table, one vertex a row, latitude and longitude in
//! decimal degrees (WGS 84), the colours to draw each map in alongside:
//!
//! ```text
//! map_id,map_number,map_name,area,vertex,latitude,longitude,stroke_color,stroke_alpha,fill_color,fill_alpha,kind
//! 32,1,Orti,1,1,44.495792,11.300521,#0d6efd,1,#0d6efd,0.15,area
//! 32,1,Orti,1,2,44.495148,11.29983,#0d6efd,1,#0d6efd,0.15,area
//! ...
//! ,,Zone vietate,1,1,44.512038,11.311477,#a626ff,1,#a626ff,1,forbidden
//! ...
//! ,,Partenza,,,44.495204,11.299989,#212529,1,#212529,1,start
//! ,,Arrivo,,,44.495178,11.30005,#212529,1,#212529,1,finish
//! ```
//!
//! `map_name`, `latitude` and `longitude` are needed; every other column
//! may be left out. The rows of one map (one `map_id`, or where there is
//! none one `map_number` and `map_name`) need not be together, and its
//! vertices are taken in the order of `vertex` within each `area`. A map
//! with a `map_number` is called `<number> - <name>`. A map with no colour,
//! or none that reads, is given one.
//!
//! `kind` says what a row is part of: a map's `area` (where there is no
//! `kind`, or one this does not know), a `forbidden` zone, or the `start` or
//! the `finish`, a single point each.
//!
//! An area's last vertex joins its first.

use std::collections::HashMap;
use std::path::Path;

use crate::bus::{csv_fields, parse_colour};

/// A colour to draw a map's area in: `#rrggbb`, and how opaque, 0 to 1.
#[derive(Clone, Debug, PartialEq)]
pub struct MapColor {
    /// The colour, `#rrggbb`.
    pub hex: String,
    /// Its opacity: 0 is not drawn at all, 1 hides what is under it.
    pub alpha: f64,
}

/// What an entry of the list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    /// A map's areas, with its name written in.
    Area,
    /// Zones the runners may not enter.
    Forbidden,
    /// Where the race starts: a point.
    Start,
    /// Where it finishes: a point.
    Finish,
}

impl MapKind {
    /// The kind a CSV row's `kind` names; [`MapKind::Area`] for any other.
    fn parse(text: &str) -> MapKind {
        match text.to_ascii_lowercase().as_str() {
            "forbidden" => MapKind::Forbidden,
            "start" => MapKind::Start,
            "finish" => MapKind::Finish,
            _ => MapKind::Area,
        }
    }

    /// Whether it is a single point rather than areas.
    pub fn is_point(self) -> bool {
        matches!(self, MapKind::Start | MapKind::Finish)
    }
}

/// One map of the list: its name, its areas, and the colours it asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct MapArea {
    /// The name as it is written on the map, e.g. `"1 - Orti"`.
    pub name: String,
    /// What it is.
    pub kind: MapKind,
    /// Each area as a closed ring of `(lon, lat)`, the first vertex repeated
    /// at the end; for a start or a finish, the one point alone.
    pub polygons: Vec<Vec<(f64, f64)>>,
    /// The outline's colour, and the name's; `None` for one of the style's.
    pub stroke: Option<MapColor>,
    /// The tint inside; `None` for a faint one of the outline's colour.
    pub fill: Option<MapColor>,
}

/// Whether `(lon, lat)` is somewhere on the earth.
fn on_earth((lon, lat): (f64, f64)) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

/// Reads the list of maps in `text`, a CSV table with one vertex a row.
pub fn parse_map_areas(text: &str) -> Result<Vec<MapArea>, String> {
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let (_, header) = lines.next().ok_or("the list is empty")?;
    let columns: HashMap<String, usize> = csv_fields(header)
        .into_iter()
        .enumerate()
        .map(|(i, c)| (c.trim().to_string(), i))
        .collect();
    for needed in ["map_name", "latitude", "longitude"] {
        if !columns.contains_key(needed) {
            return Err(format!("no {needed} column"));
        }
    }
    let column = |name: &str| columns.get(name).copied();

    // Each map's key and what it is made of, in the order first met: its
    // areas by number, each vertex with its number.
    type Vertices = Vec<(f64, (f64, f64))>;
    type Read = (String, MapArea, Vec<(String, Vertices)>);
    let mut maps: Vec<Read> = Vec::new();
    for (number, line) in lines {
        let number = number + 1;
        let fields = csv_fields(line);
        let field = |name: &str| {
            column(name)
                .and_then(|i| fields.get(i))
                .map(|f| f.trim())
                .unwrap_or("")
        };
        let name = field("map_name");
        let label = match field("map_number") {
            "" => name.to_string(),
            n => format!("{n} - {name}"),
        };
        let kind = MapKind::parse(field("kind"));
        // Without a map_id, a start and a finish of the same name apart.
        let key = match field("map_id") {
            "" => format!("{kind:?} {label}"),
            id => id.to_string(),
        };
        let degrees = |name: &str| {
            field(name)
                .parse::<f64>()
                .map_err(|_| format!("line {number}: no {name} in {line:?}"))
        };
        let at = (degrees("longitude")?, degrees("latitude")?);
        if !on_earth(at) {
            return Err(format!("line {number}: {line:?} is not on the earth"));
        }
        let color = |color: &str, alpha: &str| {
            let hex = parse_colour(field(color))?;
            let alpha = field(alpha).parse::<f64>().unwrap_or(1.0).clamp(0.0, 1.0);
            Some(MapColor { hex, alpha })
        };
        let index = match maps.iter().position(|(k, _, _)| *k == key) {
            Some(index) => index,
            None => {
                maps.push((
                    key,
                    MapArea {
                        name: label,
                        kind,
                        polygons: Vec::new(),
                        stroke: color("stroke_color", "stroke_alpha"),
                        fill: color("fill_color", "fill_alpha"),
                    },
                    Vec::new(),
                ));
                maps.len() - 1
            }
        };
        let areas = &mut maps[index].2;
        let area = field("area").to_string();
        let vertices = match areas.iter().position(|(a, _)| *a == area) {
            Some(i) => &mut areas[i].1,
            None => {
                areas.push((area, Vec::new()));
                &mut areas.last_mut().expect("just pushed").1
            }
        };
        // Without a vertex number, in the order of the rows.
        let order = field("vertex")
            .parse::<f64>()
            .unwrap_or(vertices.len() as f64);
        vertices.push((order, at));
    }
    let mut maps: Vec<MapArea> = maps
        .into_iter()
        .map(|(_, mut map, areas)| {
            map.polygons = areas
                .into_iter()
                .map(|(_, mut vertices)| {
                    vertices.sort_by(|a, b| a.0.total_cmp(&b.0));
                    vertices.into_iter().map(|(_, at)| at).collect()
                })
                .collect();
            map
        })
        .collect();
    for map in &mut maps {
        if map.kind.is_point() {
            // The first point given, should there be more.
            map.polygons.truncate(1);
            map.polygons[0].truncate(1);
            continue;
        }
        for polygon in &mut map.polygons {
            if polygon.len() < 3 {
                return Err(format!(
                    "map {:?}: an area of {} vertices",
                    map.name,
                    polygon.len()
                ));
            }
            if polygon.first() != polygon.last() {
                polygon.push(polygon[0]);
            }
        }
    }
    Ok(maps)
}

/// Reads the list of maps in the file at `path`.
pub fn read_map_areas(path: &Path) -> Result<Vec<MapArea>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_map_areas(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "\
map_id,map_number,map_name,area,vertex,latitude,longitude,stroke_color,stroke_alpha,fill_color,fill_alpha
32,1,Orti,1,1,44.495792,11.300521,#0d6efd,1,#0d6efd,0.15
32,1,Orti,1,3,44.494842,11.300366,#0d6efd,1,#0d6efd,0.15
32,1,Orti,1,2,44.495148,11.29983,#0d6efd,1,#0d6efd,0.15
28,9,\"Certosa, sud\",1,1,44.1,11.1,#DC3545,0.8,,
28,9,\"Certosa, sud\",1,2,44.2,11.1,#DC3545,0.8,,
28,9,\"Certosa, sud\",1,3,44.2,11.2,#DC3545,0.8,,
28,9,\"Certosa, sud\",2,1,44.3,11.1,#DC3545,0.8,,
28,9,\"Certosa, sud\",2,2,44.4,11.1,#DC3545,0.8,,
28,9,\"Certosa, sud\",2,3,44.4,11.2,#DC3545,0.8,,
";

    #[test]
    fn reads_the_csv_table_with_its_colours() {
        let maps = parse_map_areas(CSV).unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].name, "1 - Orti");
        // In the order of the vertex numbers, not of the rows.
        assert_eq!(
            maps[0].polygons[0],
            vec![
                (11.300521, 44.495792),
                (11.29983, 44.495148),
                (11.300366, 44.494842),
                (11.300521, 44.495792),
            ]
        );
        let blue = MapColor {
            hex: "#0d6efd".to_string(),
            alpha: 1.0,
        };
        assert_eq!(maps[0].stroke, Some(blue));
        assert_eq!(maps[0].fill.as_ref().unwrap().alpha, 0.15);
        assert_eq!(maps[1].name, "9 - Certosa, sud");
        assert_eq!(maps[1].polygons.len(), 2);
        assert_eq!(maps[1].stroke.as_ref().unwrap().hex, "#dc3545");
        assert_eq!(maps[1].stroke.as_ref().unwrap().alpha, 0.8);
        assert_eq!(maps[1].fill, None);
    }

    #[test]
    fn closes_every_area() {
        let maps = parse_map_areas(CSV).unwrap();
        for ring in maps.iter().flat_map(|m| &m.polygons) {
            assert_eq!(ring.first(), ring.last());
        }
        assert_eq!(maps[1].polygons[1].len(), 4);
    }

    #[test]
    fn reads_forbidden_zones_and_the_start_and_finish() {
        let csv = "\
map_id,map_number,map_name,area,vertex,latitude,longitude,stroke_color,stroke_alpha,fill_color,fill_alpha,kind
32,1,Orti,1,1,44.495792,11.300521,#0d6efd,1,#0d6efd,0.15,area
32,1,Orti,1,2,44.495148,11.29983,#0d6efd,1,#0d6efd,0.15,area
32,1,Orti,1,3,44.494842,11.300366,#0d6efd,1,#0d6efd,0.15,area
,,Zone vietate,1,1,44.512038,11.311477,#a626ff,1,#a626ff,1,forbidden
,,Zone vietate,1,2,44.514272,11.312764,#a626ff,1,#a626ff,1,forbidden
,,Zone vietate,1,3,44.513523,11.315553,#a626ff,1,#a626ff,1,forbidden
,,Partenza,,,44.495204,11.299989,#212529,1,#212529,1,start
,,Arrivo,,,44.495178,11.30005,#212529,1,#212529,1,finish
";
        let maps = parse_map_areas(csv).unwrap();
        let kinds: Vec<(&str, MapKind)> = maps.iter().map(|m| (m.name.as_str(), m.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("1 - Orti", MapKind::Area),
                ("Zone vietate", MapKind::Forbidden),
                ("Partenza", MapKind::Start),
                ("Arrivo", MapKind::Finish),
            ]
        );
        assert_eq!(maps[1].polygons[0].len(), 4);
        assert_eq!(maps[2].polygons, vec![vec![(11.299989, 44.495204)]]);
        assert_eq!(maps[1].fill.as_ref().unwrap().alpha, 1.0);
    }

    #[test]
    fn refuses_an_area_too_small() {
        let error = parse_map_areas("map_name,latitude,longitude\nA,44.1,11.1\nA,44.2,11.1\n");
        assert!(error.unwrap_err().contains("2 vertices"));
    }

    #[test]
    fn refuses_a_table_without_the_columns_it_needs() {
        let error = parse_map_areas("map_name,lat,lon\nA,44.1,11.1\n").unwrap_err();
        assert!(error.contains("no latitude column"), "{error}");
        assert!(parse_map_areas("").is_err());
    }

    #[test]
    fn refuses_a_csv_row_without_coordinates() {
        let error = parse_map_areas("map_name,latitude,longitude\nA,44.1,\n").unwrap_err();
        assert!(error.contains("longitude"), "{error}");
    }
}
