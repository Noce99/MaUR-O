//! Trains a U-Net to read a rendered orienteering map back into the symbols
//! it was drawn with.
//!
//! ```text
//! train [OPTIONS] <dataset> [run]
//! ```
//!
//! The dataset is a folder `generate_maps_dataset --just-opaque-areas` wrote:
//! `images/` for what the network is shown, `gt/` for what it should say, and
//! `classes.json` for what the channels of an answer mean — which is where
//! the number of classes comes from, so it is not an option here. A map whose
//! labels were left out of `gt/` is not left out of the run: they are
//! computed straight from `maps/<name>.omap` instead, at the cost of parsing
//! and rasterizing that map every time they are asked for rather than reading
//! a `.bin` — see [`maur_o_net::data`].
//!
//! ```bash
//! cargo run --release --bin generate_maps_dataset -- \
//!     --just-opaque-areas -n 300 maps/ISOM_10k.omap dataset
//! cargo run --release -p maur-o-net --bin train -- dataset runs/first
//! ```
//!
//! The run folder holds the checkpoints, the metrics of every epoch, the
//! configuration the run was started with, and `model` — the trained weights
//! — at the end of it.
//!
//! The backend is chosen when this is built, not when it is run: `ndarray` by
//! default, which is pure Rust and slow; `--features wgpu` for any GPU with a
//! Vulkan, Metal or DX12 driver; `--features cuda` for an NVIDIA one.
//!
//! Exit codes: 0 success, 1 usage error, 2 the run could not be started or
//! could not finish.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o_net::data::{CROP, DEFAULT_CROPS_PER_MAP};
use maur_o_net::training::{train, TrainingConfig, DEFAULT_ANGLE_WEIGHT};
use maur_o_net::unet::{DEFAULT_BASE_CHANNELS, DEPTH};

/// Where a run is written when no folder is named.
const DEFAULT_RUN: &str = "run";

#[derive(Parser)]
#[command(
    name = "train",
    version,
    about = "Trains a U-Net to read a rendered orienteering map back into the symbols it was \
             drawn with.\n\n\
             The dataset is what `generate_maps_dataset --just-opaque-areas` writes: images/ is \
             what the network is shown, gt/ what it should say, and classes.json says how many \
             symbols there are to tell apart. A map left out of gt/ is not left out of the run: \
             its labels are computed from maps/ instead.\n\n\
             A map is 1650 pixels square and a U-Net is not, so training runs on crops taken at \
             random from inside them -- which is also most of what stands in for having more \
             maps."
)]
struct Args {
    /// The dataset folder: images/, gt/ and classes.json under it.
    dataset: PathBuf,

    /// Where the checkpoints, the metrics and the trained weights go. Made if
    /// it is not there.
    #[arg(default_value = DEFAULT_RUN)]
    run: PathBuf,

    /// How many passes over the dataset.
    #[arg(short = 'e', long, default_value_t = 10, value_name = "COUNT")]
    epochs: usize,

    /// How many crops in one step.
    #[arg(short = 'b', long, default_value_t = 8, value_name = "CROPS")]
    batch_size: usize,

    /// How many pixels square a crop is. Must divide by sixteen, which is
    /// what a U-Net of four levels halves and doubles.
    #[arg(short = 'c', long, default_value_t = CROP, value_name = "PIXELS")]
    crop: usize,

    /// How many crops one pass takes from each map.
    #[arg(long, default_value_t = DEFAULT_CROPS_PER_MAP, value_name = "COUNT")]
    crops_per_map: usize,

    /// The feature maps at full resolution, doubling at every step down. The
    /// paper's is 64; this is a first model and starts lower.
    #[arg(long, default_value_t = DEFAULT_BASE_CHANNELS, value_name = "COUNT")]
    base_channels: usize,

    /// The step size Adam starts from.
    #[arg(short = 'l', long, default_value_t = 1.0e-4, value_name = "RATE")]
    learning_rate: f64,

    /// What the angle term counts for beside the class term.
    #[arg(long, default_value_t = DEFAULT_ANGLE_WEIGHT, value_name = "WEIGHT")]
    angle_weight: f64,

    /// The share of the maps kept for training; the rest validate against it.
    /// Split by map rather than by crop, so a validation score is a score for
    /// reading a map rather than for remembering one.
    #[arg(long, default_value_t = 0.8, value_name = "SHARE")]
    train_share: f64,

    /// How many threads decode maps ahead of the arithmetic.
    #[arg(short = 'w', long, default_value_t = 4, value_name = "COUNT")]
    workers: usize,

    /// What the crop positions and the shuffling come out of. The same seed
    /// gives the same run.
    #[arg(short = 's', long, default_value_t = 0)]
    seed: u64,

    /// Show a live terminal dashboard of the metrics in place of the plain,
    /// scrolling log -- a full-screen view of the loss, the accuracy and the
    /// angle error as they come in, epoch by epoch.
    #[arg(long)]
    dashboard: bool,
}

/// How many opaque areas the dataset holds, out of the `classes.json` the
/// generator wrote beside it.
///
/// Read rather than asked for: it is the symbol set which decides how many
/// classes there are, a network built for the wrong number would be a network
/// trained against labels it cannot express, and the file is right there.
fn classes_of(dataset: &std::path::Path) -> Result<usize, String> {
    let path = dataset.join(maur_o::dataset::CLASSES_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {}: {e}\nA dataset to train on is what generate_maps_dataset writes \
             with --just-opaque-areas; without that flag there are no labels and no {}.",
            path.display(),
            maur_o::dataset::CLASSES_FILE,
        )
    })?;

    // One number out of a small file the generator wrote, rather than a JSON
    // parser and the dependency under it.
    let key = "\"classes\":";
    let at = text
        .find(key)
        .ok_or_else(|| format!("{} names no class count", path.display()))?;
    text[at + key.len()..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| digits.parse().ok())
        .ok_or_else(|| format!("{} names no class count", path.display()))
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

    for (value, name) in [
        (args.epochs, "--epochs"),
        (args.batch_size, "--batch-size"),
        (args.crop, "--crop"),
        (args.crops_per_map, "--crops-per-map"),
        (args.base_channels, "--base-channels"),
    ] {
        if value == 0 {
            return Err((
                ExitCode::from(1),
                format!("Error: Invalid value for {name}: it must be greater than zero."),
            ));
        }
    }
    if args.crop % (1 << DEPTH) != 0 {
        return Err((
            ExitCode::from(1),
            format!(
                "Error: Invalid value for --crop: {} does not divide by {}, which is what a \
                 U-Net of {DEPTH} levels halves and doubles.",
                args.crop,
                1 << DEPTH,
            ),
        ));
    }
    if !(0.0..1.0).contains(&args.train_share) || args.train_share <= 0.0 {
        return Err((
            ExitCode::from(1),
            "Error: Invalid value for --train-share: it must be above zero and below one, so \
             that there is something to validate against."
                .to_string(),
        ));
    }
    if args.learning_rate <= 0.0 || args.learning_rate.is_nan() {
        return Err((
            ExitCode::from(1),
            "Error: Invalid value for --learning-rate: it must be greater than zero.".to_string(),
        ));
    }

    let classes =
        classes_of(&args.dataset).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;

    let config = TrainingConfig::new(classes)
        .with_epochs(args.epochs)
        .with_batch_size(args.batch_size)
        .with_workers(args.workers)
        .with_learning_rate(args.learning_rate)
        .with_base_channels(args.base_channels)
        .with_crop(args.crop)
        .with_crops_per_map(args.crops_per_map)
        .with_train_share(args.train_share)
        .with_angle_weight(args.angle_weight)
        .with_seed(args.seed);

    println!(
        "{BACKEND}: a U-Net of {DEPTH} levels from {} channels, {classes} symbols and the frame \
         to tell apart, and the sine and the cosine of the pattern angle",
        args.base_channels,
    );

    train::<Backend>(&args.dataset, &args.run, config, device(), args.dashboard)
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}

// Which backend was built in. burn takes it as a type parameter rather than
// as a setting, so it is a compile time choice, and these are the three the
// crate's features offer. CUDA first and the pure Rust one last: where more
// than one is on, the fastest is what was meant.
#[cfg(feature = "cuda")]
mod backend {
    /// What the tool prints for the backend it was built against.
    pub const BACKEND: &str = "CUDA";
    /// The backend, with a gradient tape over it.
    pub type Backend = burn::backend::Autodiff<burn::backend::Cuda>;
    /// The device it runs on.
    pub fn device() -> burn::backend::cuda::CudaDevice {
        Default::default()
    }
}

#[cfg(all(feature = "wgpu", not(feature = "cuda")))]
mod backend {
    /// What the tool prints for the backend it was built against.
    pub const BACKEND: &str = "wgpu";
    /// The backend, with a gradient tape over it.
    pub type Backend = burn::backend::Autodiff<burn::backend::Wgpu>;
    /// The device it runs on.
    pub fn device() -> burn::backend::wgpu::WgpuDevice {
        Default::default()
    }
}

#[cfg(not(any(feature = "cuda", feature = "wgpu")))]
mod backend {
    /// What the tool prints for the backend it was built against.
    pub const BACKEND: &str = "ndarray (pure Rust, and slow: build with --features wgpu or \
                               --features cuda for a real run)";
    /// The backend, with a gradient tape over it.
    pub type Backend = burn::backend::Autodiff<burn::backend::NdArray>;
    /// The device it runs on.
    pub fn device() -> burn::backend::ndarray::NdArrayDevice {
        Default::default()
    }
}

use backend::{device, Backend, BACKEND};
