//! `mauro`: every MaUR-O tool behind one command.
//!
//! ```text
//! mauro render           render a map to an image (PNG/BMP/TIFF/JPEG) or to SVG
//! mauro benchmark create build a benchmark archive from maps and a ground truth renderer
//! mauro benchmark run    render a benchmark archive and report the differences
//! mauro dem              extract an elevation GeoTIFF from a map's contours
//! mauro dem-eval         score a predicted DEM GeoTIFF against a ground-truth one
//! mauro osm              convert OpenStreetMap data into a map
//! mauro ortho            add a map's aerial orthophoto to it as a template
//! ```
//!
//! Each subcommand is documented in its own module, exit codes included;
//! `mauro <subcommand> --help` prints its options. A usage error is exit
//! code 1 for every subcommand, and so is `mauro` given no subcommand.

mod benchmark_create;
mod benchmark_run;
mod dem;
mod dem_eval;
mod ortho;
mod osm;
mod render;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "mauro",
    version,
    propagate_version = true,
    arg_required_else_help = true,
    about = "Map Utils in Rust for Orienteering: renders, benchmarks, converts and \
             analyses OpenOrienteering Mapper and OCAD maps."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

// One value per run, parsed once: how much larger `osm`'s options are than
// the others' costs nothing worth boxing them for.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    Render(render::Args),
    /// Builds and runs benchmark archives against a ground truth renderer.
    #[command(subcommand)]
    Benchmark(BenchmarkCmd),
    Dem(dem::Args),
    DemEval(dem_eval::Args),
    Osm(osm::Args),
    Ortho(ortho::Args),
}

#[derive(Subcommand)]
enum BenchmarkCmd {
    Create(benchmark_create::Args),
    Run(benchmark_run::Args),
}

fn main() -> ExitCode {
    // Parsed in two steps, not with `Cli::try_parse`, because `osm` needs its
    // own `ArgMatches` to tell an option given from one left at its default.
    let parsed = Cli::command()
        .try_get_matches()
        .and_then(|matches| Cli::from_arg_matches(&matches).map(|cli| (cli, matches)));
    let (cli, matches) = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            // clap prints its own usage/help text: --help and --version are
            // a success, anything else a usage error.
            e.print().ok();
            return ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 });
        }
    };
    match cli.command {
        Cmd::Render(args) => render::main(args),
        Cmd::Benchmark(BenchmarkCmd::Create(args)) => benchmark_create::main(args),
        Cmd::Benchmark(BenchmarkCmd::Run(args)) => benchmark_run::main(args),
        Cmd::Dem(args) => dem::main(args),
        Cmd::DemEval(args) => dem_eval::main(args),
        Cmd::Osm(args) => {
            let osm_matches = matches
                .subcommand_matches("osm")
                .expect("the osm subcommand was parsed from these matches");
            osm::main(args, osm_matches)
        }
        Cmd::Ortho(args) => ortho::main(args),
    }
}
