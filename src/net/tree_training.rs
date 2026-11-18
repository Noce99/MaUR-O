//! Training a [`TreeNet`] on a dataset `mauro net dataset` made.
//!
//! [`crate::net::training`] is burn's learner over crops of full-size maps,
//! which is what a per-pixel U-Net wants. A tree network wants something
//! else: the **whole picture**, small, because what it answers is where the
//! cuts run across it, and a crop of a map has cuts running off its edges. So
//! this is its own loop, with the same run folder, the same job folder
//! ([`Monitor`]) and the same `best.bpk`, and nothing else shared.
//!
//! What a run does:
//!
//! * reads every picture in `images/` and its labels in `gt/`, both reduced to
//!   `size` pixels square by taking the pixel at the middle of each cell, and
//!   holds them in memory — a dataset of a few thousand is some hundreds of
//!   megabytes at most;
//! * splits them by picture, the share `train_share` for training;
//! * takes `epochs` passes over the training half, `batch_size` pictures a
//!   step, annealing the temperature of the soft cuts from
//!   `temperature_start` to `temperature_end` over the whole run — warm, the
//!   cuts feel each other and the first of them are found; cold, they are
//!   the cuts the vectors will be (see [`crate::net::tree`]);
//! * validates after every epoch, keeping the weights which validated best.
//!
//! The run folder holds `tree.json` (this configuration), `architecture.txt`,
//! the dataset's `classes.json` and symbol set, `best.bpk`, and `valid.png`:
//! the first validation pictures as *picture | label | answer*, which is the
//! quickest way to see what the net reads.
//!
//! The answer is a tree of curves, not a map: `mauro net image-to-map` turns
//! it into one, as polygons and without going through pixels (see
//! [`crate::tree_vector`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use burn::config::Config;
use burn::module::Module;
use burn::optim::{AdamConfig, GradientsParams};
use burn::tensor::{Device, Int, Tensor, TensorData};
use image::imageops::FilterType;

use crate::ground_truth::{GroundTruth, BACKGROUND};
use crate::net::monitor::Monitor;
use crate::net::predict::{load_weights, save_weights};
use crate::net::training::{copy_dataset_notes, run_directory};
use crate::net::tree::{TreeNet, TreeNetConfig};

/// The name of the weights of the epoch which validated best, without the
/// extension burn adds.
const BEST_WEIGHTS: &str = "best";

/// How many validation pictures [`TreeTrainingConfig`]'s `valid.png` shows.
const PANEL_ROWS: usize = 6;

/// What a run is set up with: written to `tree.json` in its folder.
#[derive(Config, Debug)]
pub struct TreeTrainingConfig {
    /// How many opaque areas the symbol set holds.
    pub classes: usize,
    /// How many passes over the training half.
    #[config(default = 10)]
    pub epochs: usize,
    /// How many pictures a step.
    #[config(default = 16)]
    pub batch_size: usize,
    /// The side of the pictures the net is shown, in pixels; divides by 16.
    #[config(default = 64)]
    pub size: usize,
    /// Cuts from the root to a leaf: `2^depth` leaves.
    #[config(default = 4)]
    pub depth: usize,
    /// Control points of each cut's Bézier.
    #[config(default = 8)]
    pub control_points: usize,
    /// Points a cut is sampled at to measure the distance to it.
    #[config(default = 24)]
    pub samples: usize,
    /// Channels of the encoder's first level.
    #[config(default = 8)]
    pub base_channels: usize,
    /// The learning rate of Adam.
    #[config(default = 1e-3)]
    pub learning_rate: f64,
    /// The temperature the soft cuts start at, in picture widths.
    #[config(default = 0.08)]
    pub temperature_start: f32,
    /// The temperature they end at.
    #[config(default = 0.012)]
    pub temperature_end: f32,
    /// The share of the pictures kept for training.
    #[config(default = 0.9)]
    pub train_share: f64,
    /// What the split and the order of the pictures come out of.
    #[config(default = 0)]
    pub seed: u64,
}

/// A picture and its labels, reduced.
pub struct Sample {
    /// `3 * size * size`, one channel after the other, in `[0, 1]`.
    image: Vec<f32>,
    /// `size * size` classes, the frame being `classes`.
    class: Vec<i64>,
}

/// Reads every picture of `dataset` and its labels, reduced to `size` square.
pub fn load_samples(dataset: &Path, size: usize, classes: usize) -> Result<Vec<Sample>, String> {
    let folder = dataset.join("images");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map_err(|e| format!("cannot read {}: {e}", folder.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "png"))
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(format!("there are no pictures in {}", folder.display()));
    }
    names
        .iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let picture = image::open(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?
                .to_rgb8();
            let small =
                image::imageops::resize(&picture, size as u32, size as u32, FilterType::Nearest);
            let mut image = vec![0.0; 3 * size * size];
            for (x, y, pixel) in small.enumerate_pixels() {
                for channel in 0..3 {
                    image[channel * size * size + y as usize * size + x as usize] =
                        pixel[channel] as f32 / 255.0;
                }
            }

            let labels = dataset.join("gt").join(format!("{stem}.bin"));
            let truth = GroundTruth::read(&labels).map_err(|e| {
                format!(
                    "{e}\nThe tree network is trained on the labels in gt/, which `mauro net \
                     dataset --just-opaque-areas` writes."
                )
            })?;
            let (height, width) = (truth.height as usize, truth.width as usize);
            let mut class = vec![0; size * size];
            for y in 0..size {
                for x in 0..size {
                    let (row, column) = (
                        (y * 2 + 1) * height / (size * 2),
                        (x * 2 + 1) * width / (size * 2),
                    );
                    let label = truth.class_of[row * width + column];
                    class[y * size + x] = if label == BACKGROUND {
                        classes as i64
                    } else {
                        label as i64
                    };
                }
            }
            Ok(Sample { image, class })
        })
        .collect()
}

fn batch(samples: &[&Sample], size: usize, device: &Device) -> (Tensor<4>, Tensor<3, Int>) {
    let n = samples.len();
    let image: Vec<f32> = samples
        .iter()
        .flat_map(|s| s.image.iter().copied())
        .collect();
    let class: Vec<i64> = samples
        .iter()
        .flat_map(|s| s.class.iter().copied())
        .collect();
    (
        Tensor::from_data(TensorData::new(image, [n, 3, size, size]), device),
        Tensor::from_data(TensorData::new(class, [n, size, size]), device),
    )
}

/// The mean loss and the share of pixels right over `samples`.
fn evaluate(
    net: &TreeNet,
    samples: &[Sample],
    size: usize,
    temperature: f32,
    device: &Device,
) -> (f64, f64) {
    let (mut loss, mut right) = (0.0, 0usize);
    for chunk in samples.chunks(16) {
        let refs: Vec<&Sample> = chunk.iter().collect();
        let (image, class) = batch(&refs, size, device);
        let answer = net.forward(image.clone());
        let probabilities = net.probabilities(&answer, temperature);
        let predicted = probabilities.argmax(1).reshape([chunk.len(), size, size]);
        right += predicted
            .equal(class.clone())
            .int()
            .sum()
            .into_scalar::<i64>() as usize;
        let value: f64 = net.loss(image, class, temperature).into_scalar::<f64>();
        loss += value * chunk.len() as f64;
    }
    let n = samples.len().max(1);
    (loss / n as f64, right as f64 / (n * size * size) as f64)
}

/// A picture of *picture | label | answer* for the first few of `samples`.
fn write_panel(
    net: &TreeNet,
    samples: &[Sample],
    size: usize,
    temperature: f32,
    device: &Device,
    path: &Path,
) -> Result<(), String> {
    let rows = samples.len().min(PANEL_ROWS);
    // A colour for a class, the same for the label and the answer.
    let palette = |class: i64| -> [u8; 3] {
        let h = (class as u32).wrapping_mul(2_654_435_761);
        [
            (h >> 8) as u8 | 64,
            (h >> 16) as u8 | 64,
            (h >> 24) as u8 | 64,
        ]
    };
    let mut panel = image::RgbImage::new((3 * size) as u32, (rows * size) as u32);
    for (row, sample) in samples.iter().take(rows).enumerate() {
        let (image, _) = batch(&[sample], size, device);
        let answer = net.forward(image);
        let predicted: Vec<i64> = net
            .probabilities(&answer, temperature)
            .argmax(1)
            .into_data()
            .convert::<i64>()
            .try_to_vec::<i64>()
            .map_err(|e| format!("cannot read the answer back: {e:?}"))?;
        for y in 0..size {
            for x in 0..size {
                let at = y * size + x;
                let rgb = [0, 1, 2].map(|c| (sample.image[c * size * size + at] * 255.0) as u8);
                let to = (row * size + y) as u32;
                panel.put_pixel(x as u32, to, image::Rgb(rgb));
                panel.put_pixel((size + x) as u32, to, image::Rgb(palette(sample.class[at])));
                panel.put_pixel(
                    (2 * size + x) as u32,
                    to,
                    image::Rgb(palette(predicted[at])),
                );
            }
        }
    }
    panel
        .save(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The tree network a run folder holds, and the configuration it was
/// trained with. `weights` is `<run>/best.bpk` where it is `None`.
pub fn load_tree(
    run: &Path,
    weights: Option<&Path>,
    device: &Device,
) -> Result<(TreeNet, TreeTrainingConfig), String> {
    let file = run.join("tree.json");
    let config = TreeTrainingConfig::load(&file).map_err(|e| {
        format!(
            "cannot read {}: {e}\nA run of the tree network is a folder `mauro net train --model \
             tree` wrote: its tree.json says how big the network it trained was.",
            file.display(),
        )
    })?;
    let default = run.join(BEST_WEIGHTS).with_extension("bpk");
    let weights = weights.unwrap_or(&default);
    let model = TreeNetConfig::new(config.classes)
        .with_size(config.size)
        .with_depth(config.depth)
        .with_control_points(config.control_points)
        .with_samples(config.samples)
        .with_base_channels(config.base_channels)
        .init(device);
    let model = load_weights(model, weights)?;
    Ok((model, config))
}

/// Trains a tree network on `dataset`, writing the run under `into`.
/// Returns the run's folder.
pub fn train_tree(
    dataset: &Path,
    into: &Path,
    config: TreeTrainingConfig,
    device: Device,
    monitor: Option<Monitor>,
) -> Result<PathBuf, String> {
    if !config.size.is_multiple_of(16) || config.size == 0 {
        return Err(format!(
            "a picture of {} pixels does not divide by 16, which is what the encoder halves",
            config.size
        ));
    }
    if config.depth == 0 || config.depth > 8 {
        return Err("the depth of the tree must be between 1 and 8".to_string());
    }
    if config.control_points < 2 || config.samples < 3 {
        return Err("a cut needs at least 2 control points and 3 samples".to_string());
    }

    // Gradients are asked for on the device, and not by a backend type.
    let device = device.autodiff();

    let run = run_directory(into, "TreeNet")?;
    config
        .save(run.join("tree.json"))
        .map_err(|e| format!("cannot write the configuration: {e}"))?;
    copy_dataset_notes(dataset, &run)?;
    if let Some(monitor) = &monitor {
        monitor.set_run(&run);
    }
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(monitor) = &monitor {
        let stop = stop.clone();
        monitor.watch_with(move || stop.store(true, Ordering::SeqCst));
    }
    let gate = monitor.as_ref().map(Monitor::gate).unwrap_or_default();

    let samples = load_samples(dataset, config.size, config.classes)?;
    let mut order: Vec<usize> = (0..samples.len()).collect();
    let mut state = config.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for i in (1..order.len()).rev() {
        order.swap(i, (next() % (i as u64 + 1)) as usize);
    }
    let train_count = ((samples.len() as f64 * config.train_share).round() as usize)
        .clamp(1, samples.len().saturating_sub(1).max(1));
    let mut samples: Vec<Option<Sample>> = samples.into_iter().map(Some).collect();
    let mut take = |range: &[usize]| -> Vec<Sample> {
        range.iter().filter_map(|&i| samples[i].take()).collect()
    };
    let train = take(&order[..train_count]);
    let valid = take(&order[train_count..]);
    println!(
        "{}: {} pictures to train on and {} to validate against, {size}x{size} pixels, {} \
         classes and the frame",
        dataset.display(),
        train.len(),
        valid.len(),
        config.classes,
        size = config.size,
    );
    println!("{}: the run", run.display());

    let mut net = TreeNetConfig::new(config.classes)
        .with_size(config.size)
        .with_depth(config.depth)
        .with_control_points(config.control_points)
        .with_samples(config.samples)
        .with_base_channels(config.base_channels)
        .init(&device);
    std::fs::write(run.join("architecture.txt"), format!("{net}\n"))
        .map_err(|e| format!("cannot write architecture.txt: {e}"))?;
    let mut optimizer = AdamConfig::new().init();

    let steps_per_epoch = train.len().div_ceil(config.batch_size).max(1);
    let total = (config.epochs * steps_per_epoch).max(1);
    let mut iteration = 0usize;
    let mut best = f64::INFINITY;
    let mut order: Vec<usize> = (0..train.len()).collect();

    'epochs: for epoch in 1..=config.epochs {
        for i in (1..order.len()).rev() {
            order.swap(i, (next() % (i as u64 + 1)) as usize);
        }
        for (step, chunk) in order.chunks(config.batch_size).enumerate() {
            gate.wait();
            if stop.load(Ordering::SeqCst) {
                break 'epochs;
            }
            let temperature = config.temperature_start
                + (config.temperature_end - config.temperature_start) * iteration as f32
                    / total as f32;
            let picked: Vec<&Sample> = chunk.iter().map(|&i| &train[i]).collect();
            let (image, class) = batch(&picked, config.size, &device);
            let loss = net.loss(image, class, temperature);
            let value: f64 = loss.clone().detach().into_scalar::<f64>();
            let grads = GradientsParams::from_grads(loss.backward(), &net);
            net = optimizer.step(config.learning_rate, net, grads);
            iteration += 1;
            if let Some(monitor) = &monitor {
                monitor.report(epoch, config.epochs, step + 1, steps_per_epoch, iteration);
                monitor.record_loss(false, epoch, iteration, value);
            }
        }

        let temperature = config.temperature_end.max(
            config.temperature_start
                + (config.temperature_end - config.temperature_start) * iteration as f32
                    / total as f32,
        );
        let checked = net.valid();
        let inner = checked.devices().first().cloned().unwrap_or_default();
        let (loss, accuracy) = if valid.is_empty() {
            (f64::NAN, f64::NAN)
        } else {
            evaluate(&checked, &valid, config.size, temperature, &inner)
        };
        let (_, train_accuracy) = evaluate(
            &checked,
            &train[..train.len().min(200)],
            config.size,
            temperature,
            &inner,
        );
        println!(
            "epoch {epoch:3}/{}: validation loss {loss:.4}, {:.1}% of pixels right \
             (training: {:.1}%)",
            config.epochs,
            accuracy * 100.0,
            train_accuracy * 100.0,
        );
        if let Some(monitor) = &monitor {
            monitor.record_loss(true, epoch, iteration, loss);
        }
        if loss.is_nan() || loss < best {
            best = if loss.is_nan() { best } else { loss };
            save_weights(net.clone(), &run.join(BEST_WEIGHTS))?;
        }
    }

    // The last word: the weights as they are when the run had nothing better
    // to keep, and a picture of what they read.
    let weights = run.join(BEST_WEIGHTS).with_extension("bpk");
    if !weights.is_file() {
        save_weights(net.clone(), &run.join(BEST_WEIGHTS))?;
    }
    let checked = net.valid();
    let inner = checked.devices().first().cloned().unwrap_or_default();
    let shown = if valid.is_empty() { &train } else { &valid };
    write_panel(
        &checked,
        shown,
        config.size,
        config.temperature_end,
        &inner,
        &run.join("valid.png"),
    )?;
    println!(
        "{}: the weights of the epoch which validated best; {}: picture | label | answer",
        weights.display(),
        run.join("valid.png").display(),
    );
    Ok(run)
}
