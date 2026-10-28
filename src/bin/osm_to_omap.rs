//! Turns OpenStreetMap data into an OpenOrienteering Mapper map that looks
//! like openstreetmap.org's own Standard map.
//!
//! ```text
//! osm_to_omap [--bbox min_lon,min_lat,max_lon,max_lat] [--details 0-4]
//!             [--bus 13,19,...]
//!             [--train S1,S2A,MEX,...]
//!             [--gtfs feed.zip ... [--day weekday|saturday|sunday | --date YYYYMMDD]]
//!             [--maps areas.csv]
//!             [--legend]
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
//! `--details` picks how much the map shows, and at which scale: carto at
//! one zoom, a pixel of it 0.28 mm on the paper.
//!
//! | level | carto zoom | scale |
//! |---|---|---|
//! | 0 | 13 | 1:24 000 |
//! | 1 | 14 | 1:17 000 |
//! | 2 | 15 | 1:10 000 |
//! | 3 (default) | 16 | 1:5 000 |
//! | 4 | 17 | 1:2 500 |
//!
//! The map is georeferenced in the UTM zone of its centre, turned so that
//! true north is up. The resolution printed at the end is the `-r` to give
//! `map_to_image` for a rendering with one pixel to a carto pixel, as on
//! openstreetmap.org.
//!
//! `--bus` draws bus lines over the map, each in its own colour: its route,
//! its stops as circles and its number at both ends. `--train` does the same
//! for train lines — and light rail, metro, tram and monorail ones,
//! Bologna's Marconi Express (`MEX`) among them — drawn wider, with larger
//! stations, and in the line's own colour where the data gives one.
//!
//! A line is looked for in the GTFS feeds given with `--gtfs` (each a
//! `.zip`, or the folder it is unpacked into; as many as needed, e.g. TPER's
//! bus network and its Marconi Express), in the order given, and where none
//! has it in the extract's own route relations. A line no source has is
//! reported and left out.
//!
//! Along each line, every so often, is written how many minutes apart its
//! buses run between 10:00 and 16:00: counted from the GTFS timetable on one
//! day (`--date`, or the `--day` the first feed runs the most services on —
//! by default a Wednesday — used for every feed), or taken from a
//! relation's `interval` tag where a mapper has set one. A GTFS line with no
//! trip at all that day is left out: the map shows that day's network.
//!
//! Lines sharing a street are drawn side by side on it, in the order of
//! their numbers, a bar across all of them at a stop: a bundle, never wider
//! than `--bundle-width` pixels (12 by default), its lines drawn thinner
//! where they would make it wider.
//!
//! Stops less than `--stop-merge` mm apart (3 by default) are drawn as one:
//! on a bundle, typically the two sides of a street, one bar, with an arrow
//! past the end on each stop's side pointing the way its buses run. A stop
//! on no bundle is a circle, an arrow on the right of the way buses run.
//!
//! `--bus-exclude` leaves out some of the bus lines `--bus` selects —
//! the night lines from `--bus all`, say.
//!
//! `--max-headway N` leaves out the bus lines which run less often than once
//! every N minutes in those hours, or not at all.
//!
//! `--maps` draws the areas of an event's maps over everything, from a CSV
//! table of their vertices and colours (see `maur_o::map_areas`): each
//! map's outline over a tint, in the colours the table gives, and its name
//! — inside the area wherever it fits whole, level or along a long thin
//! area, and just beside it where it does not.
//!
//! `--legend` adds a legend under the map, in Italian: a sample of every
//! symbol the map uses with what it means, the bus and train lines first.
//!
//! Exit codes: 0 success, 1 usage error, 2 the OSM file, the GTFS feed or
//! the list of maps could not be read, or held nothing to convert, 4 the
//! map could not be written.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::bus::{
    gtfs_references, lines_from_gtfs, lines_from_osm, osm_references, BusLine, Day, Mode,
    HEADWAY_HOURS,
};
use maur_o::map_areas::read_map_areas;
use maur_o::osm::{read_osm, BBox};
use maur_o::osm_convert::convert;
use maur_o::osm_style::{carto_style_with_overlays, line_color, BusLook, Details};

#[derive(Parser)]
#[command(
    name = "osm_to_omap",
    version,
    about = "Converts OpenStreetMap data (.osm) into an OpenOrienteering Mapper map drawn \
             like openstreetmap.org's Standard style."
)]
struct Args {
    /// The OpenStreetMap XML file to convert.
    osm_file: PathBuf,

    /// The map to be written. Defaults to the OSM file name with a .omap suffix.
    omap_file: Option<PathBuf>,

    /// Only convert this area: min_lon,min_lat,max_lon,max_lat in degrees.
    #[arg(long, value_parser = BBox::parse, allow_hyphen_values = true)]
    bbox: Option<BBox>,

    /// How much the map shows: 0 is carto's zoom 13 at 1:24000, 1 zoom 14
    /// at 1:17000, 2 zoom 15 at 1:10000, 3 zoom 16 at 1:5000, 4 zoom 17 at
    /// 1:2500.
    #[arg(long, default_value_t = 3, value_name = "0-4",
          value_parser = clap::value_parser!(u8).range(0..=i64::from(Details::MAX)))]
    details: u8,

    /// Draw these bus lines, by number: e.g. 13,19,27. `all` draws every bus
    /// line of the GTFS feeds (or, with none, of the OSM file) which reaches
    /// the map.
    #[arg(long, value_delimiter = ',')]
    bus: Vec<String>,

    /// Leave out these bus lines from those `--bus` selects: e.g. `--bus all
    /// --bus-exclude N1,N2,FBP`.
    #[arg(
        long,
        value_delimiter = ',',
        alias = "bus_exclude",
        value_name = "LINES"
    )]
    bus_exclude: Vec<String>,

    /// Take the lines from this GTFS feed (.zip or unpacked folder) before
    /// the OSM file's route relations. May be given several times: each line
    /// comes from the first feed that has it.
    #[arg(long)]
    gtfs: Vec<PathBuf>,

    /// The day to count the GTFS headways on, YYYYMMDD. Defaults to the
    /// typical `--day` the first feed runs the most services on.
    #[arg(long, requires = "gtfs")]
    date: Option<String>,

    /// The kind of day to count the GTFS headways on: a working day (a
    /// Wednesday), a Saturday, or a Sunday (which holidays run like). GTFS
    /// lines with no trip that day are left out.
    #[arg(long, value_enum, requires = "gtfs", conflicts_with = "date")]
    day: Option<Day>,

    /// Draw these train lines (and light rail, tram, monorail), by number:
    /// e.g. S1,S2A,MEX. `all` draws every one which reaches the map.
    #[arg(long, value_delimiter = ',')]
    train: Vec<String>,

    /// Leave out every bus line that runs less often than once every this
    /// many minutes between 10:00 and 16:00: the "ogni N min" written along
    /// it. A line from a GTFS feed with no trips in those hours is left out
    /// too; one whose frequency nothing says (OSM, with no `interval` tag) is
    /// kept. Train lines are not affected.
    #[arg(long, value_name = "MINUTES")]
    max_headway: Option<u32>,

    /// The widest a bundle of bus or train lines sharing a street may be,
    /// in carto pixels (0.28 mm each on the paper): wider, and all its lines
    /// are drawn thinner. A bus line alone is 3 pixels wide, a train 4.5.
    #[arg(long, value_name = "PIXELS", default_value_t = 12.0)]
    bundle_width: f64,

    /// How close two stops on one bundle (the two sides of a street, say),
    /// or two stops off any, may be on the paper before they are drawn as
    /// one, in mm.
    #[arg(long, value_name = "MM", default_value_t = 3.0, alias = "stop_merge")]
    stop_merge: f64,

    /// Draw the areas of these maps, each with its name: a CSV table, one
    /// vertex a row, with columns map_id, map_number, map_name, area,
    /// vertex, latitude, longitude, stroke_color, stroke_alpha, fill_color
    /// and fill_alpha (map_name, latitude and longitude needed).
    #[arg(long, value_name = "FILE", alias = "areas")]
    maps: Option<PathBuf>,

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
    let maps = match &args.maps {
        Some(path) => {
            read_map_areas(path).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?
        }
        None => Vec::new(),
    };
    let numbers = |list: &[String]| -> Vec<String> {
        list.iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect()
    };
    let (bus_refs, train_refs) = (numbers(&args.bus), numbers(&args.train));
    // `all`: every line of that kind the feeds have, or where none is given,
    // every one the extract's route relations have.
    let every = |refs: &[String], mode: Mode| -> Result<(Vec<String>, bool), (ExitCode, String)> {
        if !(refs.len() == 1 && refs[0].eq_ignore_ascii_case("all")) {
            return Ok((refs.to_vec(), false));
        }
        let mut all: Vec<String> = Vec::new();
        for feed in &args.gtfs {
            let found = gtfs_references(feed, mode)
                .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
            all.extend(found);
        }
        if args.gtfs.is_empty() {
            all = osm_references(&data, mode);
        }
        all.sort_by(|a, b| maur_o::bus::line_order(a, b));
        all.dedup();
        Ok((all, true))
    };
    let (mut bus_refs, all_buses) = every(&bus_refs, Mode::Bus)?;
    let excluded = numbers(&args.bus_exclude);
    for reference in &excluded {
        if !bus_refs.contains(reference) {
            eprintln!("Warning: bus line {reference} to leave out is not among those selected");
        }
    }
    bus_refs.retain(|r| !excluded.contains(r));
    let (train_refs, all_trains) = every(&train_refs, Mode::Train)?;
    // The area is settled before the lines are, so that `all` keeps only
    // the lines which reach it.
    let bbox = args.bbox.or_else(|| data.extent()).ok_or((
        ExitCode::from(2),
        "Error: the extract holds nothing to convert".to_string(),
    ))?;
    // Each line from the first feed that has it; the first feed's day for
    // every feed's headways, so that they are counted on the same day.
    let mut headway_date = args.date.clone();
    let mut found: Vec<BusLine> = Vec::new();
    // The lines a feed has but runs no trip of that day: found, so that no
    // other source stands in for them, but not drawn.
    let mut idle: Vec<(Mode, String)> = Vec::new();
    let has = |found: &[BusLine], mode: Mode, reference: &str| {
        found
            .iter()
            .any(|l| l.mode == mode && l.reference == reference)
    };
    for feed in &args.gtfs {
        let missing: Vec<(String, Mode)> = bus_refs
            .iter()
            .map(|r| (r.clone(), Mode::Bus))
            .chain(train_refs.iter().map(|r| (r.clone(), Mode::Train)))
            .filter(|(r, mode)| !has(&found, *mode, r))
            .collect();
        if missing.is_empty() {
            break;
        }
        let references: Vec<String> = missing.iter().map(|(r, _)| r.clone()).collect();
        let day = args.day.unwrap_or_default();
        let read = lines_from_gtfs(feed, &references, headway_date.as_deref(), day, Mode::Bus)
            .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
        if headway_date.is_none() {
            headway_date = read.date;
        }
        for (mut line, runs) in read
            .lines
            .into_iter()
            .map(|l| (l, true))
            .chain(read.idle.into_iter().map(|l| (l, false)))
        {
            if let Some((_, mode)) = missing.iter().find(|(r, _)| *r == line.reference) {
                line.mode = *mode;
                if !runs {
                    idle.push((line.mode, line.reference.clone()));
                }
                found.push(line);
            }
        }
    }
    if let (Some(day), None, false) = (args.day, &headway_date, args.gtfs.is_empty()) {
        eprintln!(
            "Warning: no {} with service in the GTFS feed: no headways counted",
            format!("{day:?}").to_lowercase()
        );
    }
    // The lines a timetable stands behind: those whose missing headway
    // means they do not run in the middle of the day at all.
    let timetabled: Vec<(Mode, String)> = found
        .iter()
        .map(|l| (l.mode, l.reference.clone()))
        .collect();
    let missing = |refs: &[String], mode: Mode| -> Vec<String> {
        refs.iter()
            .filter(|r| !has(&found, mode, r))
            .cloned()
            .collect()
    };
    let osm_bus = lines_from_osm(&data, &missing(&bus_refs, Mode::Bus), Mode::Bus);
    let osm_train = lines_from_osm(&data, &missing(&train_refs, Mode::Train), Mode::Train);
    found.extend(osm_bus);
    found.extend(osm_train);
    // In the order asked for: the buses, then the trains.
    let transit: Vec<BusLine> = bus_refs
        .iter()
        .map(|r| (r, Mode::Bus))
        .chain(train_refs.iter().map(|r| (r, Mode::Train)))
        .filter_map(|(r, mode)| {
            found
                .iter()
                .find(|l| l.mode == mode && &l.reference == r)
                .cloned()
        })
        .collect();
    // Of `all`, only the lines with some of their route or a stop on the map.
    let on_map = |line: &BusLine| {
        line.route
            .iter()
            .chain(&line.detours)
            .flatten()
            .any(|&p| bbox.contains(p))
            || line.stops.iter().any(|stop| bbox.contains(stop.at))
    };
    let transit: Vec<BusLine> = transit
        .into_iter()
        .filter(|line| {
            let all = match line.mode {
                Mode::Bus => all_buses,
                Mode::Train => all_trains,
            };
            !all || on_map(line)
        })
        .collect();
    for (refs, mode, all) in [
        (&bus_refs, Mode::Bus, all_buses),
        (&train_refs, Mode::Train, all_trains),
    ] {
        if all {
            continue;
        }
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
    // Not the lines which do not run that day.
    let (transit, resting): (Vec<BusLine>, Vec<BusLine>) = transit
        .into_iter()
        .partition(|line| !idle.contains(&(line.mode, line.reference.clone())));
    if !resting.is_empty() {
        let numbers: Vec<&str> = resting.iter().map(|l| l.reference.as_str()).collect();
        println!(
            "Left out {} lines not running on {}: {}",
            resting.len(),
            headway_date.as_deref().unwrap_or("that day"),
            numbers.join(", ")
        );
    }
    // The cap on how seldom a bus may run.
    let transit: Vec<BusLine> = match args.max_headway {
        None => transit,
        Some(cap) => {
            let (kept, dropped): (Vec<BusLine>, Vec<BusLine>) =
                transit.into_iter().partition(|line| {
                    if line.mode != Mode::Bus {
                        return true;
                    }
                    match line.headway {
                        Some(minutes) => minutes.round() <= f64::from(cap),
                        None => {
                            let known = timetabled.contains(&(line.mode, line.reference.clone()));
                            if !known {
                                eprintln!(
                                    "Warning: bus {} has no known frequency, kept",
                                    line.reference
                                );
                            }
                            !known
                        }
                    }
                });
            if !dropped.is_empty() {
                let numbers: Vec<&str> = dropped.iter().map(|l| l.reference.as_str()).collect();
                println!(
                    "Left out {} bus lines running less often than every {cap} min: {}",
                    dropped.len(),
                    numbers.join(", ")
                );
            }
            kept
        }
    };
    // A train line keeps its own colour where it has one other than white;
    // every other line takes the next colour of the palette.
    let mut next_color = 0;
    let looks: Vec<BusLook> = transit
        .iter()
        .map(|line| BusLook {
            mode: line.mode,
            reference: line.reference.clone(),
            color: match (&line.color, line.mode) {
                (Some(color), Mode::Train) if color != "#ffffff" => color.clone(),
                _ => {
                    next_color += 1;
                    line_color(next_color - 1)
                }
            },
        })
        .collect();

    let details = Details::new(args.details).expect("clap checks the range");
    let map_names: Vec<String> = maps.iter().map(|m| m.name.clone()).collect();
    let style = carto_style_with_overlays(&looks, &maps, details);
    let mut conversion = convert(&data, &style, Some(bbox))
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    conversion.add_bus_lines(&style, &transit, &data, args.bundle_width, args.stop_merge);
    for name in conversion.add_map_areas(&style, &maps) {
        eprintln!("Warning: map {name} has no area on the map, left out");
    }
    if args.legend {
        // The kind of day the headways were counted on, an exact date's own.
        let day = match &headway_date {
            Some(date) if args.date.is_some() => Day::of(date).unwrap_or_default(),
            _ => args.day.unwrap_or_default(),
        };
        conversion.add_legend(&style, &transit, day);
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
    if !maps.is_empty() {
        println!("Maps: {}", map_names.join(", "));
    }
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
        "One pixel to a carto pixel, as on openstreetmap.org: map_to_image -r {:.4} {}",
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
