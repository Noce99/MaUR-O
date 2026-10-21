//! Turns OpenStreetMap data into an OpenOrienteering Mapper map that looks
//! like openstreetmap.org's own Standard map at zoom 16.
//!
//! ```text
//! osm_to_omap [--bbox min_lon,min_lat,max_lon,max_lat] <osm-file> [omap-file]
//! ```
//!
//! The input is an `.osm` XML file, as openstreetmap.org's export, the
//! Overpass API or `osmium` write one. With no `--bbox` the whole extract is
//! converted: its `<bounds>`, or, without one, the area its tagged nodes
//! cover, so that a river or a motorway the extract keeps whole does not
//! stretch the map far past it (see `OsmData::extent`). The output defaults
//! to the input's name with `.omap`.
//!
//! The map is georeferenced in the UTM zone of its centre, turned so that
//! true north is up, at the scale where one zoom-16 tile pixel is 0.28 mm on
//! the paper. The resolution printed at the end is the `-r` to give
//! `map_to_image` for a rendering that matches openstreetmap.org's tiles
//! pixel for pixel.
//!
//! Exit codes: 0 success, 1 usage error, 2 the OSM file could not be read or
//! held nothing to convert, 4 the map could not be written.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::osm::{read_osm, BBox};
use maur_o::osm_convert::convert;
use maur_o::osm_style::carto_style;

#[derive(Parser)]
#[command(
    name = "osm_to_omap",
    version,
    about = "Converts OpenStreetMap data (.osm) into an OpenOrienteering Mapper map drawn \
             like openstreetmap.org's Standard style at zoom 16."
)]
struct Args {
    /// The OpenStreetMap XML file to convert.
    osm_file: PathBuf,

    /// The map to be written. Defaults to the OSM file name with a .omap suffix.
    omap_file: Option<PathBuf>,

    /// Only convert this area: min_lon,min_lat,max_lon,max_lat in degrees.
    #[arg(long, value_parser = BBox::parse, allow_hyphen_values = true)]
    bbox: Option<BBox>,
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            e.print().ok();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };
    let omap_file = args
        .omap_file
        .clone()
        .unwrap_or_else(|| args.osm_file.with_extension("omap"));

    let data = read_osm(&args.osm_file).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    let style = carto_style();
    let conversion = convert(&data, &style, args.bbox)
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    conversion
        .write(&style, &omap_file)
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;

    let bbox = conversion.bbox;
    let (areas, lines, points) = conversion.counts;
    println!(
        "{}: map scale 1:{}, EPSG:{}, area {},{},{},{}",
        omap_file.display(),
        conversion.scale(),
        conversion.projection.georeference.epsg,
        bbox.min_lon,
        bbox.min_lat,
        bbox.max_lon,
        bbox.max_lat,
    );
    println!("{areas} areas, {lines} lines, {points} points");
    println!(
        "To match openstreetmap.org's tiles: map_to_image -r {:.4} {}",
        conversion.tile_resolution(),
        omap_file.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
