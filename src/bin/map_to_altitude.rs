//! Reads a map's contours and writes the ground under them: a raster with an
//! altitude in meters in every pixel.
//!
//! ```text
//! map_to_altitude [-e meters] [-r px-per-meter] <map-file> [altitude-file]
//! ```
//!
//! The output is a single band 32-bit float TIFF, one altitude in meters per
//! pixel, which is what GDAL, QGIS and rasterio read as a terrain model
//! without being told anything. `--preview` writes a false colour picture of
//! the same thing beside it, shaded as a relief map is shaded, which is how a
//! person checks that the answer is the terrain they expected.
//!
//! `--walls` writes the other picture worth having: the contours as they were
//! rasterized and had their loose ends closed, which is the thing everything
//! else here is worked out from. Black is the map's own line, blue an end
//! carried out to the edge of the raster, red a gap closed between two ends
//! which faced one another, and a yellow arrow the way each loose end was
//! heading. It is what to look at when contours are reported as dividing
//! nothing, or when a hillside comes out flat.
//!
//! Form lines (symbol 103) are left out. A form line is not a contour: it
//! shows what the ground does between two of them and stops as soon as it
//! has, so it closes nothing and divides nothing.
//!
//! Altitudes are relative. A contour map fixes differences in height and
//! nothing else — no map file records the height of a single line — so the
//! lowest pixel is put at nought and everything else measured up from it.
//! `--base` puts it somewhere else.
//!
//! The contour interval has to come from somewhere: `--equidistance`, or the
//! map's own notes where the mapper wrote it down there. The interval stated
//! in an ISOM symbol's *description* is not read; it is boilerplate shipped
//! with the symbol set, says "5 metres" on a map drawn at 2.5, and would put
//! a wrong number on every altitude without ever looking wrong.
//!
//! ```text
//! map_to_altitude -e 5 maps/forest_sample.omap --preview relief.png
//! ```
//!
//! Which way the ground runs is worked out from slope lines, from contours
//! closed inside the map, and from the fact that a hillside goes on climbing
//! — see [`maur_o::altitude`] for how the three fit together. Where a map has
//! none of them the shape is still right but its sense is a guess, which is
//! said so on stderr; `--invert` turns it over.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read or holds
//! no contours, 3 the raster geometry is invalid, 4 the output could not be
//! written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::altitude::{
    false_color, map_to_altitude, save_tiff, Settings, DEFAULT_BRIDGE, DEFAULT_FRAME,
    DEFAULT_RESOLUTION, DEFAULT_SEAL,
};

#[derive(Parser)]
#[command(
    name = "map_to_altitude",
    version,
    about = "Turns the contours of an OpenOrienteering Mapper map into a raster of \
             altitudes: one 32-bit float of meters per pixel, as a single band TIFF.\n\n\
             Heights are relative to the lowest ground in the map, since a contour map \
             records differences in height and never an absolute one. The contour interval \
             comes from --equidistance, or from the map's notes if it says there.\n\n\
             Which way the ground runs is read from slope lines, from contours closed \
             inside the map, and from the fact that a slope goes on sloping. A map with \
             none of those says which shape the ground is but not which way up; that is \
             reported, and --invert turns it over."
)]
struct Args {
    /// The map whose contours are to be read (.omap, .xmap).
    map_file: PathBuf,

    /// The raster to be written, a single band 32-bit float TIFF of meters.
    /// Defaults to the map file name with a .tif suffix.
    altitude_file: Option<PathBuf>,

    /// Meters of altitude between one contour and the next. Read from the
    /// map's notes when not given, and there is no default: a guessed
    /// interval scales every height in the output.
    #[arg(short = 'e', long, value_name = "METERS")]
    equidistance: Option<f64>,

    /// Resolution of the raster, in pixels per meter on the ground.
    #[arg(short = 'r', long, default_value_t = DEFAULT_RESOLUTION, value_name = "PX_PER_M")]
    resolution: f64,

    /// Also write a false colour, hill-shaded picture of the result, for a
    /// person to look at. The suffix selects the format (.png, .jpg, .tif).
    #[arg(short = 'p', long, value_name = "IMAGE")]
    preview: Option<PathBuf>,

    /// The altitude to put the lowest ground at, in meters.
    #[arg(short = 'b', long, default_value_t = 0.0, value_name = "METERS")]
    base: f64,

    /// Turn the answer upside down: every hill a hollow. For a map with
    /// nothing in it to say which way the ground runs.
    #[arg(short = 'i', long)]
    invert: bool,

    /// Also write a picture of the contours as they were rasterized, once
    /// their loose ends have been closed: black where the map drew them, blue
    /// where an end was carried out to the edge of the raster (--seal), red
    /// where two ends facing one another across a gap were joined (--bridge),
    /// and a yellow arrow on every loose end showing the way it was heading,
    /// which is what the joining is decided on. This is what everything else
    /// is worked out from, and what to look at when contours are reported as
    /// dividing nothing.
    #[arg(short = 'w', long, value_name = "IMAGE")]
    walls: Option<PathBuf>,

    /// Ground left around the contours, in meters. Nought by default, and
    /// worth leaving there: a contour cut off at the edge of the map has to
    /// reach the edge of the raster to go on dividing the ground it divides.
    #[arg(short = 'f', long, default_value_t = DEFAULT_FRAME, value_name = "METERS")]
    frame: f64,

    /// How far the loose end of a contour may be from the edge of the raster
    /// and still be taken for one the map was cut through, in meters. Such an
    /// end is carried out to the edge.
    #[arg(short = 's', long, default_value_t = DEFAULT_SEAL, value_name = "METERS")]
    seal: f64,

    /// How far a contour end which reached no edge may be from another
    /// contour and still be joined to it, in meters. This is what closes a
    /// map cut along anything but a rectangle; nought joins nothing.
    #[arg(short = 'B', long, default_value_t = DEFAULT_BRIDGE, value_name = "METERS")]
    bridge: f64,

    /// Say nothing on stderr but errors.
    #[arg(short = 'q', long)]
    quiet: bool,
}

/// The default output path: beside the map file, with a .tif suffix.
fn default_output_path(map_path: &Path) -> PathBuf {
    map_path.with_extension("tif")
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // clap prints its own usage/help text; forward its exit behavior.
            e.print().ok();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };

    if !args.resolution.is_finite() || args.resolution <= 0.0 {
        return Err((
            ExitCode::from(1),
            format!("Error: Invalid value for --resolution: {}", args.resolution),
        ));
    }
    if let Some(equidistance) = args.equidistance {
        if !equidistance.is_finite() || equidistance <= 0.0 {
            return Err((
                ExitCode::from(1),
                format!("Error: Invalid value for --equidistance: {equidistance}"),
            ));
        }
    }
    if !args.frame.is_finite() || args.frame < 0.0 {
        return Err((
            ExitCode::from(1),
            format!("Error: Invalid value for --frame: {}", args.frame),
        ));
    }
    if !args.seal.is_finite() || args.seal < 0.0 {
        return Err((
            ExitCode::from(1),
            format!("Error: Invalid value for --seal: {}", args.seal),
        ));
    }
    if !args.bridge.is_finite() || args.bridge < 0.0 {
        return Err((
            ExitCode::from(1),
            format!("Error: Invalid value for --bridge: {}", args.bridge),
        ));
    }

    let settings = Settings {
        resolution: args.resolution,
        equidistance: args.equidistance,
        frame: args.frame,
        seal: args.seal,
        bridge: args.bridge,
        base: args.base,
        invert: args.invert,
        walls: args.walls.is_some(),
    };

    let ground = map_to_altitude(&args.map_file, &settings)
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;

    if !args.quiet {
        for warning in &ground.warnings {
            eprintln!("Warning: {warning}");
        }
        let (found, idle) = ground.contours;
        let mends = ground.mends;
        if ground.form_lines > 0 {
            eprintln!(
                "{} form lines were left out: a form line shows what the ground does between \
                 two contours and is not one itself",
                ground.form_lines
            );
        }
        eprintln!(
            "{} contours ({} used, {} ends sealed, {} gaps bridged, {} ends left open), \
             {} m interval, {}x{} px at {} px/m, {:.1} to {:.1} m",
            found,
            found - idle,
            mends.sealed,
            mends.bridged,
            mends.unmatched,
            ground.equidistance,
            ground.width,
            ground.height,
            ground.resolution,
            ground.range.0,
            ground.range.1,
        );
    }

    let output = args
        .altitude_file
        .unwrap_or_else(|| default_output_path(&args.map_file));
    save_tiff(&ground, &output).map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;

    if let (Some(path), Some(walls)) = (&args.walls, &ground.walls) {
        walls.save(path).map_err(|e| {
            (
                ExitCode::from(4),
                format!("Error: cannot write {}: {e}", path.display()),
            )
        })?;
    }

    if let Some(preview) = &args.preview {
        false_color(&ground).save(preview).map_err(|e| {
            (
                ExitCode::from(4),
                format!("Error: cannot write {}: {e}", preview.display()),
            )
        })?;
    }

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
