//! Turns OpenStreetMap data into an OpenOrienteering Mapper map that looks
//! like openstreetmap.org's own Standard map at zoom 16.
//!
//! ```text
//! osm_to_omap [--bbox min_lon,min_lat,max_lon,max_lat] [--bus 13,19,...]
//!             [--gtfs feed.zip] <osm-file> [omap-file]
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
//! `--bus` draws bus lines over the map, each in its own colour: its route,
//! its stops as circles and its number at both ends. They come from the
//! extract's own route relations, or with `--gtfs` from a GTFS feed (its
//! `.zip`, or the folder it is unpacked into). A line neither has is
//! reported and left out.
//!
//! Exit codes: 0 success, 1 usage error, 2 the OSM file or the GTFS feed
//! could not be read, or held nothing to convert, 4 the map could not be
//! written.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::bus::{lines_from_gtfs, lines_from_osm};
use maur_o::osm::{read_osm, BBox};
use maur_o::osm_convert::convert;
use maur_o::osm_style::{carto_style_with_bus_lines, BusLook, BUS_COLORS};

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

    /// Draw these bus lines, by number: e.g. 13,19,27.
    #[arg(long, value_delimiter = ',')]
    bus: Vec<String>,

    /// Take the bus lines from this GTFS feed (.zip or unpacked folder)
    /// rather than from the OSM file's route relations.
    #[arg(long, requires = "bus")]
    gtfs: Option<PathBuf>,
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
    let references: Vec<String> = args
        .bus
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    let bus_lines = match &args.gtfs {
        Some(feed) => lines_from_gtfs(feed, &references)
            .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?,
        None => lines_from_osm(&data, &references),
    };
    for reference in &references {
        if !bus_lines.iter().any(|l| &l.reference == reference) {
            eprintln!("Warning: no bus line {reference} found, left out");
        }
    }
    let looks: Vec<BusLook> = bus_lines
        .iter()
        .enumerate()
        .map(|(i, line)| BusLook {
            reference: line.reference.clone(),
            color: BUS_COLORS[i % BUS_COLORS.len()].to_string(),
        })
        .collect();

    let style = carto_style_with_bus_lines(&looks);
    let mut conversion = convert(&data, &style, args.bbox)
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    conversion.add_bus_lines(&style, &bus_lines);
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
    for (line, look) in bus_lines.iter().zip(&looks) {
        println!(
            "bus {} ({}): {} stops, {}",
            line.reference,
            look.color,
            line.stops.len(),
            line.name
        );
    }
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
