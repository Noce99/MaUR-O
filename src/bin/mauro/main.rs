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
//! mauro net dataset      generate a dataset of random maps, with their pixel labels
//! mauro net grid-to-map  turn a grid of labels back into a map
//! mauro net train        train a network to read a rendered map back into its symbols
//! mauro net image-to-map read a picture of a map back into a map with a trained network
//! mauro net show-batches draw the batches a run would be trained on
//! ```
//!
//! Each subcommand is documented in its own module, exit codes included;
//! `mauro <subcommand> --help` prints its options. A usage error is exit
//! code 1 for every subcommand.
//!
//! `mauro` given no arguments at all, in a terminal, opens a terminal user
//! interface instead ([`tui`]): a screen per tool, which builds and runs the
//! same command lines. Given no arguments anywhere else — in a script, its
//! input or output redirected — it prints its help and exits with 1, as a
//! usage error, rather than wait for keys no one will press.

mod benchmark_create;
mod benchmark_run;
mod defaults;
mod dem;
mod dem_eval;
mod net_dataset;
mod net_describe;
mod net_grid_to_map;
mod net_image_to_map;
mod net_show_batches;
mod net_train;
mod ortho;
mod osm;
mod render;
mod tui;

use clap::{FromArgMatches, Parser, Subcommand};
use std::io::IsTerminal;
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
    /// Makes a dataset of random maps and trains a network to read them back.
    #[command(subcommand)]
    Net(NetCmd),
}

#[derive(Subcommand)]
enum NetCmd {
    Dataset(net_dataset::Args),
    Describe(net_describe::Args),
    GridToMap(net_grid_to_map::Args),
    Train(net_train::Args),
    ImageToMap(net_image_to_map::Args),
    ShowBatches(net_show_batches::Args),
}

#[derive(Subcommand)]
enum BenchmarkCmd {
    Create(benchmark_create::Args),
    Run(benchmark_run::Args),
}

fn main() -> ExitCode {
    if std::env::args_os().len() == 1
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        return tui::run();
    }
    // Parsed in two steps, not with `Cli::try_parse`, because `osm` needs its
    // own `ArgMatches` to tell an option given from one left at its default.
    let parsed = defaults::command()
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
        Cmd::Net(NetCmd::Dataset(args)) => net_dataset::main(args),
        Cmd::Net(NetCmd::Describe(args)) => net_describe::main(args),
        Cmd::Net(NetCmd::GridToMap(args)) => net_grid_to_map::main(args),
        Cmd::Net(NetCmd::Train(args)) => net_train::main(args),
        Cmd::Net(NetCmd::ImageToMap(args)) => net_image_to_map::main(args),
        Cmd::Net(NetCmd::ShowBatches(args)) => net_show_batches::main(args),
    }
}
