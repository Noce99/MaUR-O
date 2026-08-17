//! What the network is scored on, and the loop which improves it.
//!
//! # Two questions, one loss
//!
//! A label answers two things about a pixel, and they are not the same kind
//! of thing. *Which symbol* is a choice out of a list, and what a choice is
//! scored with is cross-entropy over the class logits — the frame's among
//! them, since "no ground cover here" is one of the answers the picture
//! really gives. *At what angle* is a direction, and what a direction is
//! scored with here is the squared error between the sine and cosine the
//! network gives and the ones the label holds.
//!
//! The two are added, the angle weighted by
//! [`TrainingConfig::angle_weight`], because they are measured in nothing
//! alike and a network left to itself would spend all its capacity on
//! whichever happened to start larger.
//!
//! The angle term is taken over **every** pixel, not only the turned ones.
//! Where there is no angle — the frame, and a symbol with no pattern to turn
//! — the label is the zero vector, and the network learning to shrink to
//! nothing there is the network learning that there is nothing to say, which
//! is worth as much as the angles themselves. On the sample sets that is most
//! of the picture: only a fifth or so of pixels carry an angle at all.
//!
//! # What is reported, and why it is five numbers
//!
//! A step here is not one classification but `batch * 256 * 256` of them,
//! and the obvious way to measure it — hand the metrics the logits and the
//! targets, as burn's own [`AccuracyMetric`](burn::train::metric::AccuracyMetric)
//! wants them — would send eighty megabytes off the device every step, on a
//! tensor whose only use is to be argmaxed. So the counting happens on the
//! device and what crosses is five scalars: the loss, how many pixels were
//! given the right symbol, how many pixels there were, how much the angles
//! agreed, and how many pixels had an angle at all. [`MapOutput`] is those
//! five, and the two metrics here divide them.
//!
//! * **Loss** — the two terms together, which is what is being minimized;
//! * **Accuracy** — the share of pixels given the right symbol, the frame
//!   counted as a symbol. A dataset is nine cells of ground in a white
//!   border, so a network which said "frame" everywhere would already score
//!   about a quarter: read it against that, not against zero;
//! * **Angle** — how far out the directions were, in degrees, over the
//!   pixels which had a direction to get wrong. Each batch's figure is
//!   `acos` of how much its predictions agreed with its labels on average,
//!   and the epoch's is those weighted by the pixels each was measured over.
//!   That is not the mean of the per-pixel angles — the angle of a mean is
//!   not the mean of the angles, and only the first can be counted on the
//!   device — so read it as one figure for how the angles are going rather
//!   than as the error of any particular pixel. Zero is perfect, ninety is a
//!   network pointing anywhere, and a batch with no rotatable pattern in it
//!   counts as one pixel of ninety rather than as a division by nothing.

use burn::config::Config;
use burn::module::Module;
use burn::nn::loss::{CrossEntropyLossConfig, MseLoss, Reduction};
use burn::optim::AdamConfig;
use burn::prelude::Backend;
use burn::record::CompactRecorder;
use burn::tensor::backend::AutodiffBackend;
use burn::tensor::Tensor;
use burn::train::metric::state::{FormatOptions, NumericMetricState};
use burn::train::metric::{
    Adaptor, ItemLazy, LossInput, LossMetric, Metric, MetricEntry, MetricMetadata, Numeric,
};
use burn::train::renderer::tui::TuiMetricsRenderer;
use burn::train::renderer::{MetricState, MetricsRenderer, TrainingProgress};
use burn::train::{LearnerBuilder, TrainOutput, TrainStep, ValidStep};

use crate::data::{MapBatch, MapBatcher, MapDataset, CROP, DEFAULT_CROPS_PER_MAP};
use crate::unet::{UNet, UNetConfig, DEFAULT_BASE_CHANNELS, DEPTH};

/// How much the angle term counts for beside the classes. See
/// [`TrainingConfig::angle_weight`].
pub const DEFAULT_ANGLE_WEIGHT: f64 = 1.0;

/// The backend the metrics run on, which is the host: a metric reads numbers
/// rather than trains on them, so everything in [`MapOutput`] is brought back
/// here before it is looked at.
type MetricBackend = burn::backend::NdArray;

/// What one step of training or validation came to — five scalars, counted on
/// the device.
///
/// Nothing here is per-pixel. The pixels were counted where they were, which
/// is what keeps a step's report to twenty bytes instead of the eighty
/// megabytes of logits they were counted out of.
pub struct MapOutput<B: Backend> {
    /// The loss, both terms together.
    pub loss: Tensor<B, 1>,
    /// How many pixels of the batch were given the right symbol.
    pub correct: Tensor<B, 1>,
    /// How many pixels the batch held.
    pub pixels: Tensor<B, 1>,
    /// How much the predicted directions agreed with the labelled ones,
    /// summed over the pixels which had one: a cosine each, so one per pixel
    /// where they point the same way and minus one where they are opposed.
    pub agreement: Tensor<B, 1>,
    /// How many pixels had an angle at all.
    pub angled: Tensor<B, 1>,
}

impl<B: Backend> ItemLazy for MapOutput<B> {
    type ItemSync = MapOutput<MetricBackend>;

    fn sync(self) -> MapOutput<MetricBackend> {
        let device = &Default::default();
        let bring = |t: Tensor<B, 1>| Tensor::from_data(t.into_data(), device);
        MapOutput {
            loss: bring(self.loss),
            correct: bring(self.correct),
            pixels: bring(self.pixels),
            agreement: bring(self.agreement),
            angled: bring(self.angled),
        }
    }
}

impl<B: Backend> Adaptor<LossInput<B>> for MapOutput<B> {
    fn adapt(&self) -> LossInput<B> {
        LossInput::new(self.loss.clone())
    }
}

impl<B: Backend> Adaptor<PixelInput<B>> for MapOutput<B> {
    fn adapt(&self) -> PixelInput<B> {
        PixelInput {
            correct: self.correct.clone(),
            pixels: self.pixels.clone(),
        }
    }
}

impl<B: Backend> Adaptor<AngleInput<B>> for MapOutput<B> {
    fn adapt(&self) -> AngleInput<B> {
        AngleInput {
            agreement: self.agreement.clone(),
            angled: self.angled.clone(),
        }
    }
}

/// The one number a scalar tensor holds.
fn scalar<B: Backend>(tensor: &Tensor<B, 1>) -> f64 {
    tensor
        .clone()
        .into_data()
        .iter::<f64>()
        .next()
        .unwrap_or(0.0)
}

/// What [`PixelAccuracyMetric`] is fed.
pub struct PixelInput<B: Backend> {
    correct: Tensor<B, 1>,
    pixels: Tensor<B, 1>,
}

/// The share of pixels given the right symbol.
#[derive(Default)]
pub struct PixelAccuracyMetric<B: Backend> {
    state: NumericMetricState,
    _backend: std::marker::PhantomData<B>,
}

impl<B: Backend> PixelAccuracyMetric<B> {
    /// Creates the metric.
    pub fn new() -> PixelAccuracyMetric<B> {
        PixelAccuracyMetric::default()
    }
}

impl<B: Backend> Metric for PixelAccuracyMetric<B> {
    type Input = PixelInput<B>;

    fn name(&self) -> String {
        "Pixel accuracy".to_string()
    }

    fn update(&mut self, input: &PixelInput<B>, _metadata: &MetricMetadata) -> MetricEntry {
        let pixels = scalar(&input.pixels);
        let share = if pixels > 0.0 {
            100.0 * scalar(&input.correct) / pixels
        } else {
            0.0
        };
        // Weighted by the pixels it was measured over, so an epoch's figure
        // is the share over its pixels rather than the mean of its batches'.
        self.state.update(
            share,
            pixels as usize,
            FormatOptions::new(self.name()).unit("%").precision(2),
        )
    }

    fn clear(&mut self) {
        self.state.reset()
    }
}

impl<B: Backend> Numeric for PixelAccuracyMetric<B> {
    fn value(&self) -> f64 {
        self.state.value()
    }
}

/// What [`AngleMetric`] is fed.
pub struct AngleInput<B: Backend> {
    agreement: Tensor<B, 1>,
    angled: Tensor<B, 1>,
}

/// What a batch with no angle in it is reported as.
///
/// A batch can hold nothing but frame and fixed patterns — a small validation
/// split of a symbol set where few fills turn, and none of the crops landed
/// on one — and the mean of no angles is not a number. Ninety degrees is: it
/// is what a network pointing anywhere scores, so a split whose angles are
/// never measured reads as uninformative rather than as perfect.
///
/// It is entered weighted as a single pixel, which is what keeps it from
/// mattering: a batch which *did* measure angles brings tens of thousands of
/// them, so one pixel of ninety degrees moves nothing. What it does is keep
/// the weights from summing to zero, and burn's summary divides by that sum.
pub const NO_ANGLE_MEASURED: f64 = 90.0;

/// How far out the pattern angles were, in degrees.
///
/// The sums are kept here rather than in a [`NumericMetricState`] for the
/// batch with no angle in it: that one has to be entered with a weight of its
/// own rather than skipped, and skipping is all a state which is only ever
/// handed a value and a count can do.
///
/// The figure is the pixel-weighted mean of the batches' angles, each batch's
/// being `acos` of how much its predictions agreed with its labels. That is
/// what burn's own summary computes from the entries this writes, so the
/// number on the progress line and the number in the table at the end are the
/// same number.
#[derive(Default)]
pub struct AngleMetric<B: Backend> {
    /// Each batch's angle times the pixels it was measured over, summed.
    weighted: f64,
    /// Those weights, summed.
    counted: f64,
    _backend: std::marker::PhantomData<B>,
}

impl<B: Backend> AngleMetric<B> {
    /// Creates the metric.
    pub fn new() -> AngleMetric<B> {
        AngleMetric::default()
    }
}

impl<B: Backend> Metric for AngleMetric<B> {
    type Input = AngleInput<B>;

    fn name(&self) -> String {
        "Angle error".to_string()
    }

    fn update(&mut self, input: &AngleInput<B>, _metadata: &MetricMetadata) -> MetricEntry {
        let (agreement, angled) = (scalar(&input.agreement), scalar(&input.angled));

        // How far out this batch was, and over how many pixels. A batch with
        // no angle in it counts as one pixel of chance — see
        // [`NO_ANGLE_MEASURED`].
        let (batch, weight) = if angled > 0.0 {
            // Rounding can put the mean a hair outside what a cosine can be,
            // and acos of that is a NaN.
            let mean = (agreement / angled).clamp(-1.0, 1.0);
            (mean.acos().to_degrees(), angled)
        } else {
            (NO_ANGLE_MEASURED, 1.0)
        };
        self.weighted += batch * weight;
        self.counted += weight;

        let measured = if angled > 0.0 {
            format!("{batch:.2} deg")
        } else {
            // Said rather than left blank: a batch with no rotatable pattern
            // in it is a thing to notice about the dataset, not a gap.
            "no angle".to_string()
        };
        MetricEntry::new(
            self.name(),
            format!("epoch {:.2} deg - batch {measured}", self.value()),
            // What burn stores a numeric entry as: this batch's value, then
            // the elements it was measured over. Per batch and not running,
            // since the summary re-aggregates these itself.
            format!("{batch},{}", weight as usize),
        )
    }

    fn clear(&mut self) {
        self.weighted = 0.0;
        self.counted = 0.0;
    }
}

impl<B: Backend> Numeric for AngleMetric<B> {
    fn value(&self) -> f64 {
        // Before the first batch there is nothing to divide, which is the one
        // moment this is asked for with no answer.
        if self.counted > 0.0 {
            self.weighted / self.counted
        } else {
            NO_ANGLE_MEASURED
        }
    }
}

impl<B: Backend> UNet<B> {
    /// One forward pass scored against the labels: the loss to descend, and
    /// the counts the metrics divide.
    pub fn forward_step(&self, batch: MapBatch<B>, angle_weight: f64) -> MapOutput<B> {
        let device = batch.image.device();
        let raw = self.forward(batch.image);
        let [size, channels, height, width] = raw.dims();
        let classes = self.classes();
        let pixels = size * height * width;

        // The channel axis last, then one row per pixel: a segmentation is a
        // classification repeated over the picture, and this is where it is
        // written out as one.
        let flat = raw.permute([0, 2, 3, 1]).reshape([pixels, channels]);
        let logits = flat.clone().slice([0..pixels, 0..classes + 1]);
        let predicted = flat.slice([0..pixels, classes + 1..classes + 3]);

        let targets = batch.class.reshape([pixels]);
        let class_loss = CrossEntropyLossConfig::new()
            .init(&device)
            .forward(logits.clone(), targets.clone());

        // The label's own pair, laid out to match: [pixels, 2].
        let wanted = batch.angle.permute([0, 2, 3, 1]).reshape([pixels, 2]);
        let angle_loss = MseLoss::new().forward(predicted.clone(), wanted.clone(), Reduction::Mean);

        // How many pixels the argmax got right, counted here rather than on
        // the host: the logits are the largest thing in this function and
        // there is no reason for them to leave the device.
        let chosen = logits.argmax(1).reshape([pixels]);
        let correct = chosen.equal(targets).float().sum();

        let (agreement, angled) = agreement(predicted, wanted);

        MapOutput {
            loss: class_loss + angle_loss.mul_scalar(angle_weight),
            correct,
            pixels: Tensor::from_data([pixels as f32], &device),
            agreement,
            angled,
        }
    }
}

/// How much the predicted directions agree with the labelled ones, summed
/// over the pixels which had one, and how many of those there were.
///
/// The length of either vector says nothing about its direction, so both are
/// normalized away: what is summed is the cosine of the angle between them,
/// one where they point the same way and minus one where they are opposed. A
/// pixel whose label is the zero vector — the frame, a symbol with no pattern
/// to turn — has no direction to agree with and contributes to neither sum.
fn agreement<B: Backend>(
    predicted: Tensor<B, 2>,
    wanted: Tensor<B, 2>,
) -> (Tensor<B, 1>, Tensor<B, 1>) {
    // A prediction shorter than this has no direction worth measuring, and
    // dividing by its length would be dividing by nothing. Clamping the
    // divisor leaves such a pixel with an agreement near zero, which is the
    // honest score for a vector pointing nowhere.
    const TINY: f32 = 1e-6;

    let length = |v: Tensor<B, 2>| v.powi_scalar(2).sum_dim(1).sqrt();
    let (predicted_length, wanted_length) = (length(predicted.clone()), length(wanted.clone()));

    // The zero vector is the label for "no angle", so its length is what
    // tells the pixels with an angle from the pixels without one. Half a
    // unit: a label is either the zero vector or a point on the unit circle,
    // with nothing in between to be caught by the wrong side of this.
    let has_angle = wanted_length.clone().greater_elem(0.5).float();

    let cosine = predicted.mul(wanted).sum_dim(1).div(
        predicted_length
            .clamp_min(TINY)
            .mul(wanted_length.clamp_min(TINY)),
    );

    let [pixels, _] = cosine.dims();
    let has_angle = has_angle.reshape([pixels]);
    (
        cosine.reshape([pixels]).mul(has_angle.clone()).sum(),
        has_angle.sum(),
    )
}

impl<B: AutodiffBackend> TrainStep<MapBatch<B>, MapOutput<B>> for UNet<B> {
    fn step(&self, batch: MapBatch<B>) -> TrainOutput<MapOutput<B>> {
        let output = self.forward_step(batch, DEFAULT_ANGLE_WEIGHT);
        TrainOutput::new(self, output.loss.backward(), output)
    }
}

impl<B: Backend> ValidStep<MapBatch<B>, MapOutput<B>> for UNet<B> {
    fn step(&self, batch: MapBatch<B>) -> MapOutput<B> {
        self.forward_step(batch, DEFAULT_ANGLE_WEIGHT)
    }
}

/// What a training run is to be.
#[derive(Config)]
pub struct TrainingConfig {
    /// How many opaque areas the symbol set holds, which the dataset's
    /// `classes.json` says.
    pub classes: usize,
    /// How many passes over the dataset.
    #[config(default = 10)]
    pub epochs: usize,
    /// How many crops in one step. A U-Net at 256 pixels square is a large
    /// tensor, so this is small.
    #[config(default = 8)]
    pub batch_size: usize,
    /// How many threads decode maps ahead of the arithmetic.
    #[config(default = 4)]
    pub workers: usize,
    /// The step size Adam starts from.
    #[config(default = 1.0e-4)]
    pub learning_rate: f64,
    /// The feature maps at full resolution — see
    /// [`crate::unet::UNetConfig::base_channels`].
    #[config(default = "DEFAULT_BASE_CHANNELS")]
    pub base_channels: usize,
    /// How many pixels square a crop is. Must divide by `2^DEPTH`.
    #[config(default = "CROP")]
    pub crop: usize,
    /// How many crops one pass takes from each map.
    #[config(default = "DEFAULT_CROPS_PER_MAP")]
    pub crops_per_map: usize,
    /// The share of the maps kept for training, the rest for validation.
    #[config(default = 0.8)]
    pub train_share: f64,
    /// What the angle term counts for beside the class term. One is a
    /// starting point rather than a finding: the two are measured in nothing
    /// alike, and which matters more is a question about the model being
    /// built rather than about the data.
    #[config(default = "DEFAULT_ANGLE_WEIGHT")]
    pub angle_weight: f64,
    /// What the crop positions and the shuffling come out of. The same seed
    /// gives the same run.
    #[config(default = 0)]
    pub seed: u64,
}

/// What `train` renders to without `--dashboard`: one line of debug output
/// per step, printed rather than drawn. Burn's own equivalent,
/// `CliMetricsRenderer`, is not exported by the crate, and building with the
/// `tui` feature on -- which the dashboard needs -- would otherwise leave
/// burn's own terminal detection picking the dashboard the moment this runs
/// somewhere with a terminal, whether `--dashboard` was asked for or not.
struct PlainRenderer;

impl MetricsRenderer for PlainRenderer {
    fn update_train(&mut self, _state: MetricState) {}
    fn update_valid(&mut self, _state: MetricState) {}

    fn render_train(&mut self, item: TrainingProgress) {
        println!("{item:?}");
    }

    fn render_valid(&mut self, item: TrainingProgress) {
        println!("{item:?}");
    }
}

/// Trains a U-Net on the dataset in `dataset`, writing the run — the
/// checkpoints, the metrics and the configuration — into `into`.
///
/// `into` is created if it is not there. `dashboard` switches the plain,
/// scrolling metrics log for a live terminal dashboard; it says nothing about
/// the run itself, which is why it is a parameter here rather than a field of
/// [`TrainingConfig`], the part of a run's setup that gets written to disk.
pub fn train<B: AutodiffBackend>(
    dataset: &std::path::Path,
    into: &std::path::Path,
    config: TrainingConfig,
    device: B::Device,
    dashboard: bool,
) -> Result<(), String> {
    if config.crop % (1 << DEPTH) != 0 {
        return Err(format!(
            "a crop of {} pixels does not divide by {}, which is what a U-Net of {DEPTH} levels \
             halves and doubles",
            config.crop,
            1 << DEPTH,
        ));
    }

    std::fs::create_dir_all(into).map_err(|e| format!("cannot make {}: {e}", into.display()))?;
    config
        .save(into.join("training.json"))
        .map_err(|e| format!("cannot write the configuration: {e}"))?;

    let all = MapDataset::load(dataset, config.seed)?
        .with_crop(config.crop)?
        .with_crops_per_map(config.crops_per_map);

    // The labels say how many symbols there are and so does `classes.json`;
    // a network built for the wrong number would train against targets it has
    // no channel for, and the cross-entropy would only say so as an assertion
    // from inside a loader thread.
    if all.classes() != config.classes {
        return Err(format!(
            "the labels in {} were written for {} symbols and this run was set up for {}",
            dataset.display(),
            all.classes(),
            config.classes,
        ));
    }

    let maps = all.maps();
    let (train, valid) = all.split(config.train_share);
    println!(
        "{}: {maps} maps, {} to train on and {} to validate against, {} crops of {} pixels from \
         each of them each pass",
        dataset.display(),
        train.maps(),
        valid.maps(),
        config.crops_per_map,
        config.crop,
    );

    // The training half runs on the autodiff backend, since that is where a
    // gradient comes from; the validation half runs on the plain one under
    // it, which is what makes a validation pass cost no tape.
    let train_loader = burn::data::dataloader::DataLoaderBuilder::<B, _, _>::new(MapBatcher)
        .batch_size(config.batch_size)
        .num_workers(config.workers)
        .shuffle(config.seed)
        .build(train);
    let valid_loader =
        burn::data::dataloader::DataLoaderBuilder::<B::InnerBackend, _, _>::new(MapBatcher)
            .batch_size(config.batch_size)
            .num_workers(config.workers)
            .build(valid);

    let mut builder = LearnerBuilder::new(into)
        .metric_train_numeric(LossMetric::<MetricBackend>::new())
        .metric_valid_numeric(LossMetric::<MetricBackend>::new())
        .metric_train_numeric(PixelAccuracyMetric::<MetricBackend>::new())
        .metric_valid_numeric(PixelAccuracyMetric::<MetricBackend>::new())
        .metric_train_numeric(AngleMetric::<MetricBackend>::new())
        .metric_valid_numeric(AngleMetric::<MetricBackend>::new())
        .with_file_checkpointer(CompactRecorder::new())
        .devices(vec![device.clone()])
        .num_epochs(config.epochs)
        .summary();
    builder = if dashboard {
        // The same interrupter the learner would otherwise make for itself,
        // so Ctrl-C still stops the run through the dashboard rather than
        // going around it.
        let interrupter = builder.interrupter();
        builder.renderer(TuiMetricsRenderer::new(interrupter, None))
    } else {
        // Chosen explicitly rather than left to burn's own terminal
        // detection, which would switch the plain log for the dashboard on
        // its own the moment this runs in a terminal -- the opposite of
        // --dashboard being what turns it on.
        builder.renderer(PlainRenderer)
    };
    let learner = builder.build(
        UNetConfig::new(config.classes)
            .with_base_channels(config.base_channels)
            .init::<B>(&device),
        AdamConfig::new().init(),
        config.learning_rate,
    );

    let trained = learner.fit(train_loader, valid_loader);

    let weights = into.join("model");
    trained
        .save_file(&weights, &CompactRecorder::new())
        .map_err(|e| format!("cannot write the weights: {e}"))?;
    println!("{}: the trained weights", weights.display());
    Ok(())
}
