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
}

impl ArchiveInfo {
    fn to_json(&self) -> String {
        format!(
            "{{\"format\":\"mot\",\"version\":{},\"layer\":\"{}\",\"grid_cm\":{},\"min_zoom\":{},\"max_zoom\":{}}}",
            crate::mot::VERSION,
            self.layer.replace('\\', "\\\\").replace('"', "\\\""),
            self.grid_cm,
            self.min_zoom,
            self.max_zoom
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
    for id in tiles.keys() {
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
    if !tiles.is_empty() {
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
                dash_offset: 0,
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
    fn something_that_is_not_ours_is_refused() {
        assert!(ArchiveInfo::from_json("{}").is_err());
        assert!(ArchiveInfo::from_json("{\"format\":\"mvt\"}").is_err());
    }
}
