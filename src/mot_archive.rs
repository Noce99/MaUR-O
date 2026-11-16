//! MOT tiles in a PMTiles v3 archive.
//!
//! PMTiles (<https://docs.protomaps.com/pmtiles/>) is a single file whose
//! directory maps a tile to a byte range, so a viewer can fetch one tile with
//! an HTTP range request from a static host. The `pmtiles` crate does the
//! container; this module puts the [`Tile`] payload of [`crate::mot`] in it,
//! compressed with zstd, and keeps what a reader needs to know about the
//! layer in the archive's metadata.
//!
//! Native only: the wasm build reads the container itself.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use pmtiles::{Compression, PmTilesReader, PmTilesWriter, TileCoord, TileType};

use crate::mot::{lat_lon, Tile, TileId};

/// What an archive says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveInfo {
    /// The symbol table the objects' symbol ids refer to, e.g. "ISOM 2017-2".
    pub layer: String,
    /// The grid of the finest tiles' nodes, in centimetres of Mercator. A
    /// coarser level has a coarser grid, which its tiles say themselves.
    pub grid_cm: u32,
    /// The coarsest zoom level the archive has.
    pub min_zoom: u8,
    /// The finest zoom level the archive has: the one where everything is kept
    /// as drawn.
    pub max_zoom: u8,
    /// Where the maps came from: for the id of each map's coverage (the object
    /// of the [`crate::mot::COVERAGE_SYMBOL`] in the tiles), the name of its
    /// file. What a coverage says about itself, as a place to look it up.
    pub sources: Vec<(u64, String)>,
}

/// `text` as a JSON string, with its quotes.
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The JSON string at the start of `rest` (after its opening quote), and what
/// follows it.
fn read_json_string(rest: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &rest[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// The `sources` of the metadata `json`.
fn read_sources(json: &str) -> Vec<(u64, String)> {
    let mut sources = Vec::new();
    let Some(at) = json.find("\"sources\":[") else {
        return sources;
    };
    let mut rest = &json[at + "\"sources\":[".len()..];
    while let Some(id_at) = rest.find("{\"id\":") {
        if rest[..id_at].contains(']') {
            break;
        }
        rest = &rest[id_at + "{\"id\":".len()..];
        let Some(end) = rest.find(',') else { break };
        let Ok(id) = rest[..end].trim().parse::<u64>() else {
            break;
        };
        let Some(name_at) = rest.find("\"name\":\"") else {
            break;
        };
        let Some((name, after)) = read_json_string(&rest[name_at + "\"name\":\"".len()..]) else {
            break;
        };
        sources.push((id, name));
        rest = after;
    }
    sources
}

impl ArchiveInfo {
    fn to_json(&self) -> String {
        let sources: Vec<String> = self
            .sources
            .iter()
            .map(|(id, name)| format!("{{\"id\":{id},\"name\":{}}}", json_string(name)))
            .collect();
        format!(
            "{{\"format\":\"mot\",\"version\":{},\"layer\":\"{}\",\"grid_cm\":{},\"min_zoom\":{},\"max_zoom\":{},\"sources\":[{}]}}",
            crate::mot::VERSION,
            self.layer.replace('\\', "\\\\").replace('"', "\\\""),
            self.grid_cm,
            self.min_zoom,
            self.max_zoom,
            sources.join(",")
        )
    }

    fn from_json(json: &str) -> Result<ArchiveInfo, String> {
        let text = |key: &str| -> Option<&str> {
            let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
            let rest = json[at..].trim_start();
            if let Some(quoted) = rest.strip_prefix('"') {
                Some(&quoted[..quoted.find('"')?])
            } else {
                let end = rest.find([',', '}'])?;
                Some(&rest[..end])
            }
        };
        if text("format") != Some("mot") {
            return Err("not a MOT archive".into());
        }
        let number = |key: &str| -> Result<u32, String> {
            text(key)
                .and_then(|v| v.trim().parse().ok())
                .ok_or(format!("the archive's metadata has no \"{key}\""))
        };
        Ok(ArchiveInfo {
            layer: text("layer")
                .ok_or("the archive's metadata has no \"layer\"")?
                .to_string(),
            grid_cm: number("grid_cm")?,
            min_zoom: number("min_zoom")? as u8,
            max_zoom: number("max_zoom")? as u8,
            sources: read_sources(json),
        })
    }
}

fn coord(id: TileId) -> Result<TileCoord, String> {
    TileCoord::new(id.z, id.x, id.y).map_err(|e| e.to_string())
}

/// Writes `tiles`, at the zoom levels `info` says, as a PMTiles archive at `path`.
pub fn write(
    path: &Path,
    tiles: &BTreeMap<TileId, Tile>,
    info: &ArchiveInfo,
) -> Result<(), String> {
    let mut west = f64::MAX;
    let mut south = f64::MAX;
    let mut east = f64::MIN;
    let mut north = f64::MIN;
    // The box of the finest level: the coarser ones have the same ground in
    // bigger tiles, so would say the archive is far larger than it is.
    for id in tiles.keys().filter(|id| id.z == info.max_zoom) {
        let size = TileId::size(id.z);
        let (x, y) = id.origin();
        (west, east) = (west.min(x), east.max(x + size));
        (south, north) = (south.min(y - size), north.max(y));
    }
    let mut builder = PmTilesWriter::new(TileType::Unknown)
        .tile_compression(Compression::Zstd)
        .min_zoom(info.min_zoom)
        .max_zoom(info.max_zoom)
        .metadata(info.to_json());
    if west < east {
        let (south_lat, west_lon) = lat_lon(west, south);
        let (north_lat, east_lon) = lat_lon(east, north);
        builder = builder.bounds(west_lon, south_lat, east_lon, north_lat);
    }
    let file = File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = builder
        .create(BufWriter::new(file))
        .map_err(|e| e.to_string())?;

    // The container wants its tiles in the order of its own tile ids, a Hilbert
    // curve, so that neighbours are stored together.
    let mut ordered: Vec<(pmtiles::TileId, TileId)> = tiles
        .keys()
        .map(|&id| Ok((pmtiles::TileId::from(coord(id)?), id)))
        .collect::<Result<_, String>>()?;
    ordered.sort_by_key(|&(hilbert, _)| u64::from(hilbert));
    for (_, id) in ordered {
        writer
            .add_tile(coord(id)?, &tiles[&id].encode())
            .map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// An archive opened for reading.
pub struct Archive {
    reader: PmTilesReader<File>,
    info: ArchiveInfo,
}

impl Archive {
    /// Opens the archive at `path`.
    pub fn open(path: &Path) -> Result<Archive, String> {
        let reader = PmTilesReader::new_with_path(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let metadata = reader.get_metadata().map_err(|e| e.to_string())?;
        let info = ArchiveInfo::from_json(&metadata)?;
        Ok(Archive { reader, info })
    }

    /// What the archive says about itself.
    pub fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    /// The file a coverage with this id was made from, where the archive says.
    pub fn source(&self, coverage_id: u64) -> Option<&str> {
        self.info
            .sources
            .iter()
            .find(|(id, _)| *id == coverage_id)
            .map(|(_, name)| name.as_str())
    }

    /// The box the archive's tiles cover, as `[west, south, east, north]` in
    /// degrees, where it says (an archive with no tiles does not).
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let h = self.reader.get_header();
        let box_ = [
            h.min_longitude,
            h.min_latitude,
            h.max_longitude,
            h.max_latitude,
        ];
        (box_[0] < box_[2] && box_[1] < box_[3]).then_some(box_)
    }

    /// The tile `id`, or `None` where the archive has nothing there.
    pub fn tile(&self, id: TileId) -> Result<Option<Tile>, String> {
        let bytes = self
            .reader
            .get_tile_decompressed(coord(id)?)
            .map_err(|e| e.to_string())?;
        bytes.map(|b| Tile::decode(b.as_ref())).transpose()
    }

    /// Every tile of zoom level `zoom` the box `[west, south, east, north]`, in
    /// Mercator meters, touches and the archive has.
    pub fn query(&self, zoom: u8, bounds: [f64; 4]) -> Result<Vec<(TileId, Tile)>, String> {
        let mut found = Vec::new();
        for id in TileId::covering(zoom, bounds) {
            if let Some(tile) = self.tile(id)? {
                found.push((id, tile));
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mot::{Node, Shape, TileObject};

    fn tile(seed: i32) -> Tile {
        let mut tile = Tile::new(5);
        for i in 0..10 {
            tile.objects.push(TileObject {
                id: u64::from(i as u32) + seed as u64 * 100,
                symbol: 3,
                rotation: 0.0,
                shape: Shape::Path(None),
                nodes: (0..5)
                    .map(|k| Node {
                        x: seed * 1000 + k * 7,
                        y: i * 11 + k,
                        flags: 0,
                    })
                    .collect(),
            });
        }
        tile
    }

    #[test]
    fn an_archive_gives_back_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pmtiles");
        let mut tiles = BTreeMap::new();
        for (x, y) in [(8700, 5900), (8701, 5900), (8700, 5901)] {
            tiles.insert(
                TileId { z: 14, x, y },
                tile(x as i32 - 8700 + y as i32 - 5900),
            );
        }
        let info = ArchiveInfo {
            layer: "ISOM 2017-2".into(),
            grid_cm: 5,
            min_zoom: 14,
            max_zoom: 14,
            sources: Vec::new(),
        };
        write(&path, &tiles, &info).unwrap();

        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.info(), &info);
        for (id, written) in &tiles {
            let read = archive.tile(*id).unwrap().unwrap();
            assert_eq!(read.objects.len(), written.objects.len());
            assert_eq!(read.objects[0].nodes, written.objects[0].nodes);
        }
        assert!(archive
            .tile(TileId { z: 14, x: 1, y: 1 })
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_query_finds_the_tiles_around_a_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pmtiles");
        let here = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let mut tiles = BTreeMap::new();
        tiles.insert(here, tile(1));
        tiles.insert(
            TileId {
                z: 14,
                x: 9000,
                y: 6000,
            },
            tile(2),
        );
        let info = ArchiveInfo {
            layer: "ISOM 2017-2".into(),
            grid_cm: 5,
            min_zoom: 14,
            max_zoom: 14,
            sources: Vec::new(),
        };
        write(&path, &tiles, &info).unwrap();
        let archive = Archive::open(&path).unwrap();
        let (west, north) = here.origin();
        let size = TileId::size(14);
        let found = archive
            .query(
                14,
                [west + 1.0, north - size + 1.0, west + 2.0, north - 1.0],
            )
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, here);
    }

    #[test]
    fn the_sources_of_the_maps_survive_the_metadata() {
        let info = ArchiveInfo {
            layer: "ISOM 2017-2".into(),
            grid_cm: 5,
            min_zoom: 0,
            max_zoom: 14,
            sources: vec![
                (7, "Corno alle Scale.omap".into()),
                (12, "with \"quotes\", a \\ and\nnewline é.omap".into()),
            ],
        };
        let json = info.to_json();
        let back = ArchiveInfo::from_json(&json).unwrap();
        assert_eq!(back.sources[0], (7, "Corno alle Scale.omap".to_string()));
        assert_eq!(back.sources[1].0, 12);
        assert_eq!(
            back.sources[1].1,
            "with \"quotes\", a \\ and\nnewline é.omap"
        );
        assert_eq!((back.min_zoom, back.max_zoom, back.grid_cm), (0, 14, 5));
        // An archive made without any says so by having none.
        let plain = ArchiveInfo {
            sources: vec![],
            ..info
        };
        assert!(ArchiveInfo::from_json(&plain.to_json())
            .unwrap()
            .sources
            .is_empty());
    }

    #[test]
    fn the_bounds_are_those_of_the_finest_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pmtiles");
        let mut tiles = BTreeMap::new();
        tiles.insert(
            TileId {
                z: 14,
                x: 8700,
                y: 5900,
            },
            tile(1),
        );
        // A coarser level's tile is much larger than the one it comes from.
        tiles.insert(
            TileId {
                z: 8,
                x: 135,
                y: 92,
            },
            tile(2),
        );
        let info = ArchiveInfo {
            layer: "ISOM 2017-2".into(),
            grid_cm: 5,
            min_zoom: 8,
            max_zoom: 14,
            sources: Vec::new(),
        };
        write(&path, &tiles, &info).unwrap();
        let [w, s, e, n] = Archive::open(&path).unwrap().bounds().unwrap();
        assert!(e - w < 0.05 && n - s < 0.05, "{w} {s} {e} {n}");
    }

    #[test]
    fn something_that_is_not_ours_is_refused() {
        assert!(ArchiveInfo::from_json("{}").is_err());
        assert!(ArchiveInfo::from_json("{\"format\":\"mvt\"}").is_err());
    }
}
