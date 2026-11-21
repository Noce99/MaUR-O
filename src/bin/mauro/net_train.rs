//! Trains a network to read a rendered orienteering map back into the symbols
//! it was drawn with: a U-Net, which says what every pixel is, or with
//! `--model tree` a tree of curves, which says where the picture is cut into
//! areas and what each one is (see [`maur_o::net::tree`]). The tree is trained
//! on whole pictures reduced to `--size`, and reads with `--depth`,
//! `--control-points`, `--samples`, `--temperature-start` and
//! `--temperature-end`; the U-Net's `--crop`, `--crops-per-map`,
//! `--angle-weight`, `--line-weight`, `--scale-weight`, `--weight-decay`,
//! `--patience`, `--overhang`, `--workers` and `--image-valid` do not apply
//! to it.
//!
//! A dataset drawn over a range of densities (`mauro net dataset
//! --min-resolution/--max-resolution`) gives the U-Net a scale head as well:
//! it learns to say how dense a picture is, which `mauro net image-to-map`
//! asks it when it is not told, and reports how far out it was as the Scale
//! error. `--scale-weight` is what that term counts for.
//!
//! ```text
//! mauro net train [OPTIONS] <dataset> [trainings]
//! ```
//!
//! The dataset is a folder `generate_maps_dataset --just-opaque-areas` wrote:
//! `images/` for what the network is shown, `gt/` for what it should say, and
//! `classes.json` for what the channels of an answer mean — which is where
//! the number of classes comes from, so it is not an option here. A map whose
//! labels were left out of `gt/` is not left out of the run: they are
//! computed straight from `maps/<name>.omap` instead, at the cost of parsing
//! and rasterizing that map every time they are asked for rather than reading
//! a `.bin` — see [`maur_o::net::data`].
//!
//! ```bash
//! cargo run --release -- net dataset \
//!     --just-opaque-areas -n 300 maps/ISOM_10k.omap dataset
//! cargo run --release -- net train dataset trainings
//! ```
//!
//! The training folder is a history rather than a run: each run writes
//! `<Model>_YYYY_MM_DD__hh_mm_ss` under it, and that folder holds
//! `training.json` and `architecture.txt` for what was trained,
//! `checkpoint/001/`, `002/`, … for what a checkpointed epoch left behind —
//! the last five and the best one, rather than all of them — `train/001/` and
//! `valid/001/` for the metrics of every epoch, `experiment.log` for what the
//! run logged, and `best.bpk` — the weights of the epoch which validated best
//! — at the end of it.
//!
//! Every epoch crops each training map somewhere new, so that a run cannot
//! learn a fixed set of crops by heart; the validation maps are cropped in the
//! same places every epoch, so that their scores compare. A U-Net run stops
//! before `--epochs` once `--patience` epochs (10 by default, `0` never) have
//! gone by without a lower validation loss: `best.bpk` is that lowest epoch,
//! so what comes after it is only overfitting.
//!
//! `--from <RUN>` starts a U-Net from a past run's weights instead of random
//! ones: a run folder, which gives its `best.bpk` (or, for a run that failed
//! or was stopped before writing one, its latest checkpoint), or a `.bpk`
//! inside a run folder. The past run must have trained the same network: the
//! same classes, `--base-channels`, line symbols and scale head. Only the
//! weights carry over. The new run gets its own folder, a fresh optimizer,
//! and a learning rate that warms up and anneals over its own `--epochs`.
//! Its `training.json` names the file it started from, under `from`.
//!
//! The backend is chosen when this is built, not when it is run: `flex` by
//! default, which is pure Rust and slow; `--features wgpu` for any GPU with a
//! Vulkan, Metal or DX12 driver; `--features cuda` for an NVIDIA one. Which
//! GPU it runs on is `--gpu` (0 by default), numbered as `nvidia-smi` numbers
//! them under CUDA.
//!
//! Started from the terminal interface, a run is detached, and reports to it
//! through the folder named by the `MAURO_JOB_DIR` environment variable
//! (see [`maur_o::net::monitor`]): its progress and losses, and a request to
//! pause or stop. Without that variable it reports to nobody, and the
//! command line is all there is to it.
//!
//! Exit codes: 0 success, 1 usage error, 2 the run could not be started or
//! could not finish.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use maur_o::dataset::{Classes, CLASSES_FILE};

use maur_o::net::monitor::Monitor;
use maur_o::net::training::{train, TrainingConfig};
use maur_o::net::tree_training::{train_tree, TreeTrainingConfig};
use maur_o::net::unet::DEPTH;

/// Which network a run trains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Model {
    /// A U-Net: a class and an angle for every pixel, trained on crops.
    Unet,
    /// A tree of curves: the picture cut in two by a curve, each half again,
    /// every leaf one symbol, trained on whole pictures reduced to `--size`.
    Tree,
}

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
pub struct Args {
    /// The dataset folder: images/, gt/ and classes.json under it.
    dataset: PathBuf,

    /// The training folder, which holds one timestamped folder per run: the
    /// checkpoints, the metrics and the weights of each go under that. Made if
    /// it is not there.
    trainings: PathBuf,

    /// Which GPU to train on, by its index: with --features cuda, CUDA's own
    /// count in the order nvidia-smi lists them (the PCI bus's, which this
    /// asks CUDA for unless CUDA_DEVICE_ORDER says otherwise); with
    /// --features wgpu, the count of the discrete GPUs wgpu finds. The flex
    /// backend runs on the CPU and ignores it.
    #[arg(long, value_name = "INDEX")]
    gpu: usize,

    /// The network to train. The U-Net reads a class for every pixel and is
    /// trained on crops of the maps; the tree answers with the curves which
    /// cut the picture into its areas and a symbol for each, and is trained on
    /// the whole pictures reduced to --size. The options of the other model
    /// are ignored: --crop, --crops-per-map, --angle-weight, --line-weight,
    /// --scale-weight, --weight-decay, --patience,
    /// --overhang, --workers and --image-valid are the U-Net's; --size,
    /// --depth, --control-points, --samples and the two temperatures the
    /// tree's.
    #[arg(long, value_enum)]
    model: Model,

    /// How many passes over the dataset. A pass of the tree is one step per
    /// --batch-size pictures, which is short: it wants tens of them.
    #[arg(short = 'e', long, value_name = "COUNT")]
    epochs: usize,

    /// How many crops in one step.
    #[arg(short = 'b', long, value_name = "CROPS")]
    batch_size: usize,

    /// How many pixels square a crop is. Must divide by sixteen, which is
    /// what a U-Net of four levels halves and doubles.
    #[arg(short = 'c', long, value_name = "PIXELS")]
    crop: usize,

    /// How many crops one pass takes from each map.
    #[arg(long, value_name = "COUNT")]
    crops_per_map: usize,

    /// The feature maps at full resolution, doubling at every step down. The
    /// paper's is 64; this is a first model and starts lower.
    #[arg(long, value_name = "COUNT")]
    base_channels: usize,

    /// The rate the warmup climbs to, which the run then anneals down from.
    /// Not the rate of any particular step: the first steps are slower while
    /// the batch normalization finds its statistics, and the last are slower
    /// again -- see `maur_o::net::training::WarmupCosine`.
    #[arg(short = 'l', long, value_name = "RATE")]
    learning_rate: f64,

    /// What the angle term counts for beside the class term. The term is
    /// taken over the pixels which have an angle and no others, so this is
    /// what an angled pixel's direction is worth beside its symbol.
    #[arg(long, value_name = "WEIGHT")]
    angle_weight: f64,

    /// What the lines term counts for beside the ground's. Only a dataset
    /// made with --line-labels has one: the network then also answers, for
    /// every line symbol, whether a line of it runs through each pixel, and
    /// reports how much of them it found as the Line IoU.
    #[arg(long, value_name = "WEIGHT")]
    line_weight: f64,

    /// What the scale term counts for beside the ground's. Only a dataset
    /// drawn over a range of densities (--min-resolution/--max-resolution)
    /// has one: the network then also says how dense a picture is, and
    /// reports how far out it was as the Scale error.
    #[arg(long, value_name = "WEIGHT")]
    scale_weight: f64,

    /// How hard the weights are pulled back towards nothing at every step, as
    /// a share of that step's own rate. A batch normalization leaves the loss
    /// with no opinion about the scale of the layer feeding it, so nothing
    /// else holds that scale down over a long run, and a run without this one
    /// validates best somewhere in the middle and comes apart after it. Much
    /// more than the default pulls the normalizations' own scale down with
    /// it, which costs more than it saves. Nought turns it off, which makes
    /// the optimizer plain Adam.
    #[arg(long, value_name = "SHARE")]
    weight_decay: f64,

    /// How many epochs without a lower validation loss the run carries on
    /// for before it stops, short of --epochs. best.bpk is the epoch with
    /// the lowest, so every epoch after it which is no better is only
    /// overfitting. Nought never stops early.
    #[arg(long, value_name = "EPOCHS")]
    patience: usize,

    /// How far a crop may hang off the edge of a map, in pixels, with what
    /// is past the edge filled in as the white paper a map is printed on.
    /// Half the crop by default, which makes the crop's centre uniform and so
    /// every pixel of the map equally likely to be trained on. Nought keeps
    /// every crop wholly inside, which sounds right and quietly under-shows
    /// whatever lives at a map's edge -- a corner drawn uniformly puts the
    /// pixels it lands on in the middle far more often than at the rim.
    #[arg(long, value_name = "PIXELS")]
    overhang: Option<usize>,

    /// The share of the maps kept for training; the rest validate against it.
    /// Split by map rather than by crop, so a validation score is a score for
    /// reading a map rather than for remembering one.
    #[arg(long, value_name = "SHARE")]
    train_share: f64,

    /// How many threads decode maps ahead of the arithmetic.
    #[arg(short = 'w', long, value_name = "COUNT")]
    workers: usize,

    /// How many of the validation split's pictures to read back into maps
    /// each epoch: they are drawn beside the originals under image_valid/, so
    /// that a run shows its work rather than only scoring it. Nought draws
    /// none, which is what a run in a hurry wants.
    #[arg(long, value_name = "COUNT")]
    image_valid: usize,

    /// What the crop positions and the shuffling come out of. The same seed
    /// gives the same run.
    #[arg(short = 's', long)]
    seed: u64,

    /// The tree's: how many pixels square the pictures are reduced to. Must
    /// divide by sixteen.
    #[arg(long, value_name = "PIXELS")]
    size: usize,

    /// The tree's: how many cuts lie between its root and a leaf. The tree has
    /// two to the power of this many leaves, so this many symbols can be told
    /// apart by it at most.
    #[arg(long, value_name = "COUNT")]
    depth: usize,

    /// The tree's: how many control points the Bézier of each cut has. More
    /// follow a wandering boundary closer.
    #[arg(long, value_name = "COUNT")]
    control_points: usize,

    /// The tree's: how many points a cut is sampled at to measure how far a
    /// pixel is from it.
    #[arg(long, value_name = "COUNT")]
    samples: usize,

    /// The tree's: how blurred its cuts are at the start of the run, in
    /// picture widths. Warm cuts feel each other, which is how the first of
    /// them are found.
    #[arg(long, value_name = "WIDTHS")]
    temperature_start: f32,

    /// The tree's: how blurred its cuts are at the end of the run, which is
    /// how sharp the curves it ends with are.
    #[arg(long, value_name = "WIDTHS")]
    temperature_end: f32,

    /// Start the U-Net from a past run's weights instead of random ones: the
    /// run's folder, which gives its best.bpk, or its latest checkpoint if
    /// it failed or was stopped before writing one; or a .bpk inside a run
    /// folder, such as checkpoint/007/model.bpk. That run must have trained
    /// the same network: the same classes, --base-channels, line symbols and
    /// scale head. Only the weights carry over. This run is a new one in
    /// its own folder, with a fresh optimizer and a learning rate that warms
    /// up and anneals over its own --epochs.
    #[arg(long, value_name = "RUN")]
    from: Option<PathBuf>,

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
/// What the dataset says about its own answers: how many classes a label
/// has, and that the symbol set naming them is there beside it.
///
/// Both are checked before a run starts rather than as it copies them, so a
/// folder which cannot be trained on is refused in the same breath as one
/// which cannot be read.
fn notes_of(dataset: &std::path::Path) -> Result<Classes, String> {
    let path = dataset.join(CLASSES_FILE);
    let classes = Classes::read(&path).map_err(|e| {
        format!(
            "{e}\nA dataset to train on is what mauro net dataset writes with \
             --content areas or --content areas-and-lines; the default, --content all, \
             draws what nothing could label, and writes no {CLASSES_FILE}.",
        )
    })?;

    // The set the classes are places in travels with the dataset and is
    // copied into the run folder, so that what the run learns can be read
    // back afterwards. A dataset without it can still be trained on, and the
    // model would be one nothing could turn into a map.
    let Some(symbol_set) = &classes.symbol_set else {
        return Err(format!(
            "{} names no symbol set. A dataset carries the set its maps were drawn with, since a \
             class of a label is a place in that set's opaque areas; this one was generated \
             before that was so, and a run off it could not be read back into a map. Generating \
             it again is what puts the set there.",
            path.display(),
        ));
    };
    let file = dataset.join(symbol_set);
    if !file.is_file() {
        return Err(format!(
            "{} names {symbol_set} as its symbol set and there is no such file beside it",
            path.display(),
        ));
    }
    Ok(classes)
}

fn run(args: Args) -> Result<(), (ExitCode, String)> {
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
    if args.model == Model::Tree {
        for (value, name) in [
            (args.size, "--size"),
            (args.depth, "--depth"),
            (args.control_points, "--control-points"),
            (args.samples, "--samples"),
        ] {
            if value == 0 {
                return Err((
                    ExitCode::from(1),
                    format!("Error: Invalid value for {name}: it must be greater than zero."),
                ));
            }
        }
        if !args.size.is_multiple_of(16) {
            return Err((
                ExitCode::from(1),
                format!(
                    "Error: Invalid value for --size: {} does not divide by 16, which is what \
                     the encoder halves.",
                    args.size,
                ),
            ));
        }
        if args.depth > 8 || args.control_points < 2 || args.samples < 3 {
            return Err((
                ExitCode::from(1),
                "Error: Invalid value for --depth, --control-points or --samples: a depth of at \
                 most 8, at least 2 control points and at least 3 samples."
                    .to_string(),
            ));
        }
    } else if !args.crop.is_multiple_of(1 << DEPTH) {
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
    if args.weight_decay < 0.0 || args.weight_decay.is_nan() {
        return Err((
            ExitCode::from(1),
            "Error: Invalid value for --weight-decay: it cannot be negative. Nought turns it off."
                .to_string(),
        ));
    }

    if args.model == Model::Tree && args.from.is_some() {
        return Err((
            ExitCode::from(1),
            "Error: --from starts a U-Net from a past run's weights; the tree network has no \
             such option."
                .to_string(),
        ));
    }

    let notes = notes_of(&args.dataset).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    let classes = notes.classes;

    if args.model == Model::Tree {
        let config = TreeTrainingConfig::new(classes)
            .with_epochs(args.epochs)
            .with_batch_size(args.batch_size)
            .with_size(args.size)
            .with_depth(args.depth)
            .with_control_points(args.control_points)
            .with_samples(args.samples)
            .with_base_channels(args.base_channels)
            .with_learning_rate(args.learning_rate)
            .with_temperature_start(args.temperature_start)
            .with_temperature_end(args.temperature_end)
            .with_train_share(args.train_share)
            .with_seed(args.seed);
        println!(
            "{BACKEND}: a tree of {} cuts of {} control points, {classes} symbols and the frame \
             to tell apart, on pictures of {} pixels",
            (1usize << args.depth) - 1,
            args.control_points,
            args.size,
        );
        let monitor = Monitor::from_env();
        let result = train_tree(
            &args.dataset,
            &args.trainings,
            config,
            device(args.gpu),
            monitor.clone(),
        )
        .map(|_| ());
        if let Some(monitor) = &monitor {
            monitor.end(&result);
        }
        return result.map_err(|e| (ExitCode::from(2), format!("Error: {e}")));
    }

    let config = TrainingConfig::new(classes)
        .with_epochs(args.epochs)
        .with_batch_size(args.batch_size)
        .with_workers(args.workers)
        .with_learning_rate(args.learning_rate)
        .with_base_channels(args.base_channels)
        .with_crop(args.crop)
        .with_crops_per_map(args.crops_per_map)
        .with_overhang(args.overhang)
        .with_train_share(args.train_share)
        .with_angle_weight(args.angle_weight)
        .with_line_weight(args.line_weight)
        .with_scale_weight(args.scale_weight)
        .with_weight_decay(args.weight_decay)
        .with_patience(args.patience)
        .with_image_valid(args.image_valid)
        .with_seed(args.seed)
        .with_from(args.from);

    println!(
        "{BACKEND}: a U-Net of {DEPTH} levels from {} channels, {classes} symbols and the frame \
         to tell apart, and the sine and the cosine of the pattern angle",
        args.base_channels,
    );
    println!(
        "  drawn with {}, which is copied into the run folder with {CLASSES_FILE} so that what \
         the run learns can be read back into a map",
        notes.symbol_set.as_deref().unwrap_or("its symbol set"),
    );

    // Told where to report by whoever started this detached — the terminal
    // interface does — and by nobody else.
    let monitor = Monitor::from_env();
    let result = train(
        &args.dataset,
        &args.trainings,
        config,
        device(args.gpu),
        args.dashboard,
        monitor.clone(),
    )
    .map(|_| ());
    if let Some(monitor) = &monitor {
        monitor.end(&result);
    }
    result.map_err(|e| (ExitCode::from(2), format!("Error: {e}")))
}

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "the run could not be started or could not finish",
        _ => return None,
    })
}

/// Runs `mauro net train`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            report_refusal(&message);
            code
        }
    }
}

/// A run refused before it started — a dataset which cannot be trained on,
/// say — has reported nothing to whoever is watching it, which would see a
/// job gone silent rather than one which failed: it is told so here. A run
/// which got as far as reporting has said how it ended itself.
fn report_refusal(message: &str) {
    let Some(dir) = std::env::var_os(maur_o::net::monitor::JOB_DIR_ENV) else {
        return;
    };
    if maur_o::net::monitor::Status::read(std::path::Path::new(&dir)).is_some() {
        return;
    }
    if let Some(monitor) = Monitor::from_env() {
        let message = message.strip_prefix("Error: ").unwrap_or(message);
        monitor.end(&Err(message.to_string()));
    }
}

// Which backend was built in. burn takes it as a type parameter rather than
// as a setting, so it is a compile time choice, and these are the three the
// crate's features offer. CUDA first and the pure Rust one last: where more
// than one is on, the fastest is what was meant.
mod backend {
    use burn::tensor::Device;

    /// What the tool prints for the backend it was built against.
    #[cfg(feature = "cuda")]
    pub const BACKEND: &str = "CUDA";
    /// What the tool prints for the backend it was built against.
    #[cfg(all(feature = "wgpu", not(feature = "cuda")))]
    pub const BACKEND: &str = "wgpu";
    /// What the tool prints for the backend it was built against.
    #[cfg(not(any(feature = "cuda", feature = "wgpu")))]
    pub const BACKEND: &str = "flex (pure Rust, and slow: build with --features wgpu or \
                               --features cuda for a real run)";

    /// GPU `gpu`, numbered as `nvidia-smi` numbers them: CUDA's own order
    /// is fastest first unless it is asked for the bus's, which is asked for
    /// here before anything starts CUDA. No gradient tape over it: the
    /// trainers ask for that themselves.
    #[cfg(feature = "cuda")]
    pub fn device(gpu: usize) -> Device {
        if std::env::var_os("CUDA_DEVICE_ORDER").is_none() {
            std::env::set_var("CUDA_DEVICE_ORDER", "PCI_BUS_ID");
        }
        Device::cuda(burn::tensor::DeviceIndex::new(gpu))
    }

    /// Discrete GPU `gpu`, in wgpu's count of them.
    #[cfg(all(feature = "wgpu", not(feature = "cuda")))]
    pub fn device(gpu: usize) -> Device {
        Device::wgpu(burn::tensor::DeviceKind::DiscreteGpu(gpu))
    }

    /// The CPU, whichever GPU was asked for.
    #[cfg(not(any(feature = "cuda", feature = "wgpu")))]
    pub fn device(_gpu: usize) -> Device {
        Device::flex()
    }

    /// Whether the backend runs on a GPU, and so whether `--gpu` means
    /// anything.
    pub const ON_GPU: bool = cfg!(any(feature = "cuda", feature = "wgpu"));
}

pub use backend::ON_GPU;
use backend::{device, BACKEND};
