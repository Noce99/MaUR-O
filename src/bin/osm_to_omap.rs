//! Turns OpenStreetMap data into an OpenOrienteering Mapper map that looks
//! like openstreetmap.org's own Standard map at zoom 16.
//!
//! ```text
//! osm_to_omap [--bbox min_lon,min_lat,max_lon,max_lat] [--bus 13,19,...]
//!             [--gtfs feed.zip [--date YYYYMMDD]] [--train S1,S2A,MEX,...] [--legend]
//!             <osm-file> [omap-file]
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
//! Along each line, every so often, is written how many minutes apart its
//! buses run between 10:00 and 16:00: counted from the GTFS timetable on one
//! working day (`--date`, or by default the Wednesday the feed runs the most
//! services on), or taken from a relation's `interval` tag where a mapper
//! has set one.
//!
//! `--train` does the same for train lines — and light rail, metro, tram and
//! monorail ones, Bologna's Marconi Express (`MEX`) among them — always from
//! the route relations, drawn wider, with larger stations, and in the line's
//! own colour where the data gives one.
//!
//! `--legend` adds a legend under the map, in Italian: a sample of every
//! symbol the map uses with what it means, the bus and train lines first.
//!
//! Exit codes: 0 success, 1 usage error, 2 the OSM file or the GTFS feed
//! could not be read, or held nothing to convert, 4 the map could not be
//! written.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::bus::{lines_from_gtfs, lines_from_osm, Mode, HEADWAY_HOURS};
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

    /// The day to count the GTFS headways on, YYYYMMDD. Defaults to the
    /// Wednesday the feed runs the most services on.
    #[arg(long, requires = "gtfs")]
    date: Option<String>,

    /// Draw these train lines (and light rail, tram, monorail), by number:
    /// e.g. S1,S2A,MEX. Always from the OSM file's route relations.
    #[arg(long, value_delimiter = ',')]
    train: Vec<String>,

    /// Add a legend under the map, explaining every symbol it uses.
    #[arg(long)]
    legend: bool,
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
    let numbers = |list: &[String]| -> Vec<String> {
        list.iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect()
    };
    let (bus_refs, train_refs) = (numbers(&args.bus), numbers(&args.train));
    let mut headway_date = None;
    let mut transit = match &args.gtfs {
        Some(feed) => {
            let read = lines_from_gtfs(feed, &bus_refs, args.date.as_deref())
                .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
            headway_date = read.date;
            read.lines
        }
        None => lines_from_osm(&data, &bus_refs, Mode::Bus),
    };
    transit.extend(lines_from_osm(&data, &train_refs, Mode::Train));
    for (refs, mode) in [(&bus_refs, Mode::Bus), (&train_refs, Mode::Train)] {
        for reference in refs {
            if !transit
                .iter()
                .any(|l| l.mode == mode && &l.reference == reference)
            {
                eprintln!(
                    "Warning: no {} line {reference} found, left out",
                    mode.key()
                );
            }
        }
    }
    // A train line keeps its own colour where it has one other than white;
    // every other line takes the next colour of the palette.
    let mut palette = BUS_COLORS.iter().cycle();
    let looks: Vec<BusLook> = transit
        .iter()
        .map(|line| BusLook {
            mode: line.mode,
            reference: line.reference.clone(),
            color: match (&line.color, line.mode) {
                (Some(color), Mode::Train) if color != "#ffffff" => color.clone(),
                _ => palette.next().expect("the palette cycles").to_string(),
            },
        })
        .collect();

    let style = carto_style_with_bus_lines(&looks);
    let mut conversion = convert(&data, &style, args.bbox)
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    conversion.add_bus_lines(&style, &transit);
    if args.legend {
        conversion.add_legend(&style, &transit);
    }
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
    if let Some(date) = &headway_date {
        println!(
            "Headways: trips leaving between {}:00 and {}:00 on {date}",
            HEADWAY_HOURS.0, HEADWAY_HOURS.1
        );
    }
    for (line, look) in transit.iter().zip(&looks) {
        let every = line
            .headway
            .map(|m| format!(", every {} min", m.round().max(1.0)))
            .unwrap_or_default();
        println!(
            "{} {} ({}): {} stops{every}, {}",
            line.mode.key(),
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
