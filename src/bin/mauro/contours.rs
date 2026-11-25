//! Draws a terrain model's contour lines into a map, at a given
//! equidistance, with the map's own contour symbols.
//!
//! ```text
//! mauro contours [--equidistance M] [--index N] [--form-lines]
//!                [--smooth M] [--min-length M] [--output FILE]
//!                <map-file> <dtm.tif>
//! ```
//!
//! The terrain model is an elevation GeoTIFF in the map's own CRS, such as
//! the one `mauro terrain` downloads for the map. The map may be an
//! OpenOrienteering Mapper `.omap`/`.xmap` file or an OCAD `.ocd` one,
//! georeferenced in a projected CRS with an EPSG code.
//!
//! Contours are drawn every `--equidistance` meters (5 by default), at
//! multiples of it, with the map's contour symbol (101). Every
//! `--index`th of them, counted from 0 m, is an index contour (102), every
//! 5th by default, none with `--index 0`. `--form-lines` adds form lines
//! (103) halfway between two contours. A map without the symbol a line
//! needs is turned away.
//!
//! The lines are drawn in blue, so they can be told from the map's own
//! contours: the map gets a colour named `generated_contours`, just above
//! the one its contours are drawn in, and a blue copy of each of those
//! symbols a line needs (same widths and dashes, same code), named
//! `generated_contours`, `generated_index_contours` and
//! `generated_form_lines`, after its other symbols. A map that has them
//! already, from an earlier run, keeps them.
//!
//! The model is blurred first, by a Gaussian `--smooth` meters wide (its
//! standard deviation, 2 by default, 0 for none), so a LiDAR survey's every
//! furrow does not become a wiggle. Lines shorter than `--min-length`
//! meters on the ground (20 by default) are left out. The lines end
//! where the model has no elevations.
//!
//! The lines go into a map part of their own, named after the equidistance
//! (`DTM contours, 5 m`), after the map's other parts. Where the map
//! already has a part of that name, it is replaced, so running again
//! replaces what the last run drew. Nothing else in the file changes. The
//! map is written beside the original with `_contours.omap` in place of its
//! extension (`forest.omap` → `forest_contours.omap`), unless `--output`
//! names another `.omap` or `.xmap` file, which may be the map itself.
//!
//! Exit codes: 0 success, 1 usage error (an output that is not an `.omap`
//! or `.xmap` file included), 2 the map could not be read, 3 the map is
//! not georeferenced in a projected CRS with an EPSG code, or has no symbol
//! for a line it needs, 4 the terrain model could not be read, is not in
//! the map's CRS, or has no elevations, 5 the map could not be written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::contour_lines::{
    add_contour_part, add_generated_symbols, contour_objects, Options, PartAdded,
};
use maur_o::dem_geo_tiff_evaluator::{read, CrsKey};
use maur_o::ocd::{is_ocd_file, ocd_to_omap_xml};
use maur_o::xml_reader::read_xml_map_str;

#[derive(Parser)]
#[command(
    about = "Draws a terrain model's contour lines into a map",
    long_about = "Draws the contour lines of a terrain model (an elevation GeoTIFF in the map's \
             CRS, such as mauro terrain downloads) into a map, at a given equidistance, with the \
             map's own contour symbols, in a map part of their own."
)]
pub struct Args {
    /// The map (.omap, .xmap or .ocd) to draw the contours into.
    map_file: PathBuf,

    /// The terrain model: an elevation GeoTIFF in the map's CRS.
    dtm: PathBuf,

    /// The vertical distance between two contours, in meters.
    #[arg(long, short, value_name = "METERS", default_value_t = 5.0)]
    equidistance: f64,

    /// Every how many contours, counted from 0 m, one is an index contour;
    /// 0 for none.
    #[arg(long, value_name = "N", default_value_t = 5)]
    index: u32,

    /// Draws form lines halfway between two contours.
    #[arg(long, alias = "form_lines")]
    form_lines: bool,

    /// How much to smooth the model first: a Gaussian's standard deviation,
    /// in meters on the ground; 0 for none.
    #[arg(long, value_name = "METERS", default_value_t = 2.0)]
    smooth: f64,

    /// The shortest line drawn, in meters on the ground.
    #[arg(
        long,
        value_name = "METERS",
        default_value_t = 20.0,
        alias = "min_length"
    )]
    min_length: f64,

    /// Where to write the map (.omap or .xmap). Defaults to the map's name
    /// with `_contours.omap`, beside it.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

/// A failure: the exit code and what to say.
type Failure = (ExitCode, String);

fn fail(code: u8, message: impl std::fmt::Display) -> Failure {
    (ExitCode::from(code), format!("Error: {message}"))
}

/// Whether `path` names a file Mapper reads as a map in its own format.
fn is_mapper_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("omap") || e.eq_ignore_ascii_case("xmap"))
}

fn run(args: Args) -> Result<(), Failure> {
    if !(args.equidistance.is_finite() && args.equidistance > 0.0) {
        return Err(fail(
            1,
            "--equidistance must be a positive number of meters",
        ));
    }
    if !(args.smooth.is_finite() && args.smooth >= 0.0) {
        return Err(fail(
            1,
            "--smooth must be zero or a positive number of meters",
        ));
    }
    if !(args.min_length.is_finite() && args.min_length >= 0.0) {
        return Err(fail(
            1,
            "--min-length must be zero or a positive number of meters",
        ));
    }
    let output = args.output.clone().unwrap_or_else(|| {
        let stem = args
            .map_file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        args.map_file
            .with_file_name(format!("{stem}_contours.omap"))
    });
    if !is_mapper_file(&output) {
        return Err(fail(
            1,
            format!(
                "{} is not a map file: the output must end in .omap or .xmap",
                output.display()
            ),
        ));
    }

    // The map.
    let map_file = &args.map_file;
    let cannot_read =
        |e: &dyn std::fmt::Display| fail(2, format!("cannot read {}: {e}", map_file.display()));
    let bytes = std::fs::read(map_file).map_err(|e| cannot_read(&e))?;
    let xml = if is_ocd_file(&bytes) {
        ocd_to_omap_xml(&bytes).map_err(|e| cannot_read(&e))?.xml
    } else {
        String::from_utf8(bytes).map_err(|e| cannot_read(&e))?
    };
    let (map, _) = read_xml_map_str(&xml).map_err(|e| cannot_read(&e))?;
    let georef = match &map.georeferencing {
        Some(g) if g.epsg != 0 => g.clone(),
        _ => {
            return Err(fail(
                3,
                "the map is not georeferenced in a projected CRS with an EPSG code",
            ))
        }
    };

    // The terrain model.
    let raster = read(&args.dtm).map_err(|e| fail(4, e))?;
    match raster.crs {
        CrsKey::Projected(epsg) if epsg as i32 == georef.epsg => {}
        CrsKey::Projected(epsg) => {
            return Err(fail(
                4,
                format!(
                    "{} is in EPSG:{epsg}, the map in EPSG:{}",
                    args.dtm.display(),
                    georef.epsg
                ),
            ))
        }
        CrsKey::Geographic(epsg) => {
            return Err(fail(
                4,
                format!(
                    "{} is in geographic coordinates (EPSG:{epsg}), the map in EPSG:{}",
                    args.dtm.display(),
                    georef.epsg
                ),
            ))
        }
        CrsKey::Unspecified => eprintln!(
            "Warning: {} names no CRS; taking it to be the map's, EPSG:{}",
            args.dtm.display(),
            georef.epsg
        ),
    }
    if raster.values.iter().all(|v| v.is_nan()) {
        return Err(fail(4, format!("{} has no elevations", args.dtm.display())));
    }

    let options = Options {
        equidistance: args.equidistance,
        index_every: args.index,
        form_lines: args.form_lines,
        smooth: args.smooth,
        min_length: args.min_length,
    };
    let (xml, symbols) = add_generated_symbols(&xml, &map, &options).map_err(|e| {
        let hint = if e.contains("index contour") {
            "; --index 0 draws none"
        } else {
            ""
        };
        fail(3, format!("{e}{hint}"))
    })?;
    let objects = contour_objects(&raster, &georef, &symbols, &options).map_err(|e| fail(3, e))?;

    let part = format!("DTM contours, {} m", args.equidistance);
    let (with_part, added) =
        add_contour_part(&xml, &part, &objects).map_err(|e| cannot_read(&e))?;
    write_atomically(&output, with_part.as_bytes())?;

    let count =
        |id: Option<i32>| id.map_or(0, |id| objects.iter().filter(|o| o.symbol_id == id).count());
    println!("Wrote {}", output.display());
    println!(
        "{} lines in part \"{part}\"{}: {} contours, {} index contours, {} form lines",
        objects.len(),
        match added {
            PartAdded::Added => "",
            PartAdded::Replaced => " (replaced)",
        },
        count(symbols.contour),
        if args.index > 0 {
            count(symbols.index)
        } else {
            0
        },
        if args.form_lines {
            count(symbols.form_line)
        } else {
            0
        },
    );
    Ok(())
}

/// Writes `path` all at once: to a temporary file beside it, then renamed
/// over it, so a failure halfway never leaves half a map. The file keeps the
/// permissions it had; a new one gets the usual ones.
fn write_atomically(path: &Path, content: &[u8]) -> Result<(), Failure> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.contours.tmp"));
    let permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    let written = std::fs::write(&temporary, content)
        .and_then(|()| match permissions {
            Some(permissions) => std::fs::set_permissions(&temporary, permissions),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    written.map_err(|e| {
        std::fs::remove_file(&temporary).ok();
        fail(5, format!("cannot write {}: {e}", path.display()))
    })
}

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error, or an output that is not an .omap or .xmap file",
        2 => "the map could not be read",
        3 => "the map is not georeferenced in a projected CRS with an EPSG code, or has no symbol for a line it needs",
        4 => "the terrain model could not be read, is not in the map's CRS, or has no elevations",
        5 => "the map could not be written",
        _ => return None,
    })
}

/// Runs `mauro contours`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
