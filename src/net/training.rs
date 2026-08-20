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
//! The angle term is taken over the pixels which **have** an angle, and over
//! no others. Where there is none — the frame, and a symbol with no pattern
//! to turn — the label is the zero vector, and asking the network to hit it
//! there was asking it to spend most of its capacity saying nothing: only a
//! fifth or so of pixels carry an angle at all, so four fifths of the term
//! was a pull towards the origin which the angles themselves had to fight.
//! Masked, the term is the mean squared error per angled pixel, and a batch
//! with no angle in it contributes nought rather than a pull.
//!
//! # What the classes are weighted by
//!
//! A dataset is mostly frame and mostly whichever ground cover is commonest,
//! and a cross-entropy which counts every pixel the same is a cross-entropy
//! whose easiest move is to answer with the commonest class everywhere. That
//! is not a hypothetical: a run left unweighted settles there and reads back
//! as an empty map. So the loss weights each class by the inverse of how much
//! of the sampled ground truth it holds — see [`class_weights`] — which makes
//! a rare symbol worth as much to get right as a common one.
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

use std::path::{Path, PathBuf};

use burn::config::Config;
use burn::data::dataset::Dataset;
use burn::grad_clipping::GradientClippingConfig;
use burn::lr_scheduler::LrScheduler;
use burn::module::{extract_type_name, Module};
use burn::optim::AdamWConfig;
use burn::prelude::Backend;
use burn::record::DefaultRecorder;
use burn::tensor::backend::AutodiffBackend;
use burn::tensor::Tensor;
use burn::train::checkpoint::{
    ComposedCheckpointingStrategy, KeepLastNCheckpoints, MetricCheckpointingStrategy,
};
use burn::train::metric::state::{FormatOptions, NumericMetricState};
use burn::train::metric::store::{Aggregate, Direction, Split};
use burn::train::metric::{
    Adaptor, ItemLazy, LossInput, LossMetric, Metric, MetricEntry, MetricMetadata, Numeric,
};
use burn::train::renderer::tui::TuiMetricsRenderer;
use burn::train::renderer::{MetricState, MetricsRenderer, TrainingProgress};
use burn::train::{LearnerBuilder, LearnerSummary, TrainOutput, TrainStep, ValidStep};
use burn::LearningRate;

use crate::dataset::{Classes, CLASSES_FILE};
use crate::symbol_kinds::Catalogue;
use crate::xml_reader::read_xml_map;

use crate::net::data::{MapBatch, MapBatcher, MapDataset, CROP, DEFAULT_CROPS_PER_MAP};
use crate::net::image_valid::{ImageValidation, DEFAULT_IMAGE_VALID, IMAGE_VALID};
use crate::net::predict::ReadBackSettings;
use crate::net::unet::{UNet, UNetConfig, DEFAULT_BASE_CHANNELS, DEPTH};

/// How much the angle term counts for beside the classes. See
/// [`TrainingConfig::angle_weight`].
///
/// A fifth rather than the whole: the angle term is a squared error and the
/// class term a cross-entropy, and on the runs this was set from the first
/// starts larger and moves faster. Reading the symbol is what the network is
/// for, and the angle is what it says about a symbol it has already found.
///
/// Nought turns the term off, and is what a run wants where the symbol set's
/// angles are not worth the gradient — but not, any longer, because they
/// cannot be learned. The target is folded by each symbol's own rotational
/// symmetry before it is asked for, which is what makes a pattern that looks
/// the same at several angles a question with one answer rather than none;
/// see [`crate::symbol_kinds::pattern_symmetry`].
pub const DEFAULT_ANGLE_WEIGHT: f64 = 0.2;

/// The backend the metrics run on, which is the host: a metric reads numbers
/// rather than trains on them, so everything in [`MapOutput`] is brought back
/// here before it is looked at.
type MetricBackend = burn::backend::NdArray;

/// The rate the warmup climbs to, and so the largest rate a run takes a step
/// at -- see [`WarmupCosine`].
///
/// Three ten-thousandths. A tenth of that trains too slowly to leave the
/// commonest class in ten epochs; ten times it trains, and then falls over
/// somewhere in the second epoch. This is between them, and it is a peak
/// reached after a warmup rather than a rate started at, which is most of
/// what makes it safe to be the larger of the two.
pub const DEFAULT_LEARNING_RATE: f64 = 3.0e-4;

/// How hard the weights are pulled back towards nothing at every step, as a
/// share of that step's own learning rate.
///
/// Not regularization in the usual sense of it. A batch normalization leaves
/// the loss **invariant** to the scale of the convolution which feeds it —
/// double those weights and the normalization divides the doubling straight
/// back out — so nothing in the loss has an opinion about how large they are,
/// and Adam, whose step is about the same size whatever the gradient was,
/// walks that scale upwards for as long as a run lasts. A fourteen epoch run
/// of this network ended with its convolution weights around a hundred times
/// the size they were initialized at, every coordinate having drifted the one
/// way for the whole run.
///
/// Decoupled, which is what makes it bite: [`AdamWConfig`] shrinks the weight
/// by `rate * this` beside the gradient step rather than adding
/// `this * weight` to the gradient, so the pull is proportional to how large
/// the weight is rather than to how large Adam's running average of the
/// gradient happens to be — which, being the divisor, would cancel most of it
/// out. That is Loshchilov and Hutter's paper, and it is exactly the case
/// here, where what is being held is a direction the loss says nothing about.
///
/// A tenth, which is what a sweep said. Four runs of six epochs over the same
/// maps, at ten times the rate so that a long run's drift falls into a few
/// hundred steps — the best pixel accuracy each of them validated at, and
/// what became of the normalizations' own scale:
///
/// ```text
/// decay    best valid    last epoch    gamma rms    gamma min
///     0          51.5%         32.6%        0.994        0.496
///  0.01          59.8%         59.8%        0.983        0.440
///   0.1          65.0%         65.0%        0.884        0.104
///   1.0          35.2%         32.2%        0.304        0.088
/// ```
///
/// Nought is the one which came apart: best at its third epoch and a third
/// worse by its sixth, which is a run whose scale ran away from its own
/// running statistics. A tenth was still climbing when it stopped.
///
/// And not more than a tenth, because burn's AdamW decays **every** parameter
/// it is handed, a batch normalization's own `gamma` among them — and `gamma`
/// is scale free in the same way the convolution is, so nothing in the loss
/// holds it up either. At one it is pulled to a third of what it should be
/// and the run learns half as much. (`running_mean` and `running_var` are a
/// `RunningState` rather than a `Param`, so they are never decayed.)
///
/// What this is **not** is what keeps a checkpoint readable. Even at a tenth
/// the pre-normalization variances still reach 1e5 over a long run; what
/// makes that harmless is writing them down in full precision, which is
/// [`BEST_WEIGHTS`]. The two are separate repairs to the same run.
///
/// Nought turns it off, and turns [`AdamWConfig`] back into plain Adam.
pub const DEFAULT_WEIGHT_DECAY: f64 = 0.1;

/// The ceiling on the norm of one step's gradient.
///
/// One, which is the usual figure and is here as a ceiling rather than as a
/// scale: an ordinary step is well under it and is left exactly as it was,
/// and only the rare step which would have moved the weights out from under
/// the batch normalization is cut down to it.
const GRADIENT_NORM: f32 = 1.0;

/// How many maps the class balance is counted over before a run starts.
///
/// The count is over whole ground truths rather than over crops, so a map is
/// a hundred thousand pixels of evidence and thirty-two of them are three
/// million: enough to put a share to two figures, which is all a weight needs
/// it to. The cost is thirty-two label reads, or thirty-two rasterizations
/// where the labels are not on disk, against an epoch of tens of thousands of
/// steps.
const BALANCE_SAMPLE: usize = 32;

/// The floor a class's share is held at before it is inverted.
///
/// A symbol which no sampled map held would otherwise be weighted by one over
/// nothing, and a class the sample caught twice by something nearly as large:
/// either would hand the whole loss to a class the network has barely seen.
/// A thousandth of the pixels is the smallest share this trusts.
const RAREST_SHARE: f64 = 1e-3;

/// What each class should count for in the cross-entropy, the frame last:
/// the inverse of the share of the ground truth it holds, scaled so that the
/// average class counts for one, and never less than one.
///
/// The inverting is what makes a rare symbol worth finding. A dataset is nine
/// cells of ground in a white border, and without it the cheapest answer is
/// the commonest class everywhere — an answer which scores well on pixels and
/// reads back as an empty map.
///
/// The floor is what stops that cure being worse than the disease: inverting
/// a share does not only lift the rare classes, it pushes the common ones
/// *down*, and a class pushed below one is a class the loss has made cheap to
/// get wrong. Nothing should be penalized for being common.
///
/// # The shares have to be the shares a crop shows
///
/// [`MapDataset::class_balance`] counts over **crops**, and it has to: a
/// network trained on crops is never shown a whole map, and the two do not
/// hold the same mixture. A crop is placed at a corner drawn uniformly, so
/// the pixel it lands on is that corner plus an offset — the sum of two
/// uniform draws, which piles up in the middle of the map and thins out at
/// its edges. On a dataset image the white frame is 33% of the picture and
/// **14%** of what 256-pixel crops show.
///
/// Counted over whole maps instead, the frame comes out weighted 0.44 — the
/// lowest of the six — for being commonest, while in the crops it is in fact
/// the *rarest*. It was worth 6% of the weighted loss where it should have
/// been worth 17%, and a run of this network duly declined to predict it at
/// all: after fourteen epochs it called 1.2% of the frame the frame and
/// filled the map's border with ground cover.
///
/// Fixing the count is necessary and may not be sufficient — a class the
/// crops under-show is still a class the crops under-show, and what
/// `image_to_map` puts through the network is a whole picture where the frame
/// is a third of it, not a crop where it is a seventh. See
/// [`MapDataset::class_balance`].
pub fn class_weights(train: &MapDataset) -> Result<Vec<f32>, String> {
    let shares = train.class_balance(BALANCE_SAMPLE)?;

    let inverted: Vec<f64> = shares
        .iter()
        .map(|&share| 1.0 / share.max(RAREST_SHARE))
        .collect();

    // Scaled to average one, so that turning the weighting on does not also
    // turn the effective learning rate up: what changes is how the loss is
    // divided between the classes, not how large it is. Then floored, which
    // is the one thing allowed to put that average back above one -- see
    // above for what it buys.
    let mean = inverted.iter().sum::<f64>() / inverted.len() as f64;
    Ok(inverted
        .into_iter()
        .map(|weight| (weight / mean).max(1.0) as f32)
        .collect())
}

/// How many steps the learning rate is warmed up over, and how far down it is
/// annealed by the end of the run.
///
/// A U-Net full of batch normalization starts with running statistics which
/// are not yet the statistics of anything, and a first step at the full rate
/// moves the weights out from under them: the training half goes on looking
/// fine, since it normalizes by the batch it is holding, and the validation
/// half — which normalizes by the running figures — comes apart. Warming up
/// is what lets the statistics catch up while the weights are still moving
/// slowly.
const WARMUP_STEPS: usize = 500;

/// What the rate is annealed down to by the last step, as a share of the rate
/// it warmed up to. Not nought: a rate of nought is a step of nothing, and
/// the last epoch may as well still be learning.
const FINAL_SHARE: f64 = 0.05;

/// A learning rate which is warmed up and then annealed: linearly from
/// nothing to `peak` over [`WARMUP_STEPS`] steps, then down a cosine to
/// [`FINAL_SHARE`] of `peak` by the last step of the run.
///
/// Both halves are here for the same reason. Adam at a rate which suits the
/// middle of a run is too large for its first steps, when the batch
/// normalization has no statistics yet and the gradients are largest, and too
/// large for its last, when what is left to do is small. A constant rate is
/// what makes a run which improves for an epoch and then falls over.
#[derive(Clone, Copy, Debug)]
pub struct WarmupCosine {
    /// The rate the warmup climbs to and the annealing comes down from.
    peak: LearningRate,
    /// How many steps the climb takes.
    warmup: usize,
    /// How many steps the whole run is, warmup included.
    total: usize,
    /// How many steps have been taken. One-based once `step` has been called,
    /// which is why it starts at nought.
    taken: usize,
}

impl WarmupCosine {
    /// The schedule for a run of `total` steps peaking at `peak`.
    ///
    /// The warmup is [`WARMUP_STEPS`] steps, or half the run where the run is
    /// shorter than twice that — a short run should not be all warmup.
    pub fn new(peak: LearningRate, total: usize) -> WarmupCosine {
        let total = total.max(1);
        WarmupCosine {
            peak,
            warmup: WARMUP_STEPS.min(total / 2).max(1),
            total,
            taken: 0,
        }
    }
}

impl LrScheduler for WarmupCosine {
    /// How many steps have been taken, which is the whole of the state.
    type Record<B: Backend> = usize;

    fn step(&mut self) -> LearningRate {
        self.taken += 1;

        if self.taken <= self.warmup {
            // Never nought on the first step: a step of nothing is a step
            // which teaches nothing, and burn asks for the rate before the
            // step rather than after it.
            return self.peak * self.taken as f64 / self.warmup as f64;
        }

        // How far through the annealing this step is, in [0, 1].
        let after = (self.taken - self.warmup) as f64;
        let length = (self.total.saturating_sub(self.warmup)).max(1) as f64;
        let through = (after / length).min(1.0);

        let cosine = 0.5 * (1.0 + (std::f64::consts::PI * through).cos());
        self.peak * (FINAL_SHARE + (1.0 - FINAL_SHARE) * cosine)
    }

    fn to_record<B: Backend>(&self) -> Self::Record<B> {
        self.taken
    }

    fn load_record<B: Backend>(mut self, record: Self::Record<B>) -> Self {
        self.taken = record;
        self
    }
}

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

/// How far out the pattern angles were, in degrees of the **folded** angle.
///
/// Folded, because that is the angle the network was asked for: a symbol
/// whose pattern looks the same every quarter turn is scored on four times
/// its angle, so ninety degrees here is a little over twenty-two on the map —
/// see [`crate::ground_truth::GroundTruth::sin_cos_folded`]. What the two
/// ends of the scale mean is unchanged by that, and they are what this is
/// read for: nought is right, and ninety is what a network pointing anywhere
/// scores, whatever the symmetry.
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
    pub fn forward_step(&self, batch: MapBatch<B>) -> MapOutput<B> {
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
        let class_loss = self.class_loss().forward(logits.clone(), targets.clone());

        // The label's own pair, laid out to match: [pixels, 2].
        let wanted = batch.angle.permute([0, 2, 3, 1]).reshape([pixels, 2]);

        // Which pixels have a direction at all. The zero vector is the label
        // for "no angle", so its length is what tells the two apart; half a
        // unit, since a label is either that or a point on the unit circle,
        // with nothing in between to be caught by the wrong side of this.
        let wanted_length = wanted.clone().powi_scalar(2).sum_dim(1).sqrt();
        let has_angle = wanted_length
            .clone()
            .greater_elem(0.5)
            .float()
            .reshape([pixels]);
        let angled = has_angle.clone().sum();

        // The squared error of the pixels which had a direction, over how
        // many of those there were: a mean per angled pixel rather than per
        // pixel. A batch with no angle in it comes to nought over one, which
        // is nought -- the term simply says nothing that step.
        let squared = predicted
            .clone()
            .sub(wanted.clone())
            .powi_scalar(2)
            .sum_dim(1)
            .reshape([pixels]);
        let angle_loss = squared.mul(has_angle.clone()).sum() / angled.clone().clamp_min(1.0);

        // How many pixels the argmax got right, counted here rather than on
        // the host: the logits are the largest thing in this function and
        // there is no reason for them to leave the device.
        let chosen = logits.argmax(1).reshape([pixels]);
        let correct = chosen.equal(targets).float().sum();

        let agreement = agreement(predicted, wanted, has_angle);

        MapOutput {
            loss: class_loss + angle_loss.mul_scalar(self.angle_weight()),
            correct,
            pixels: Tensor::from_data([pixels as f32], &device),
            agreement,
            angled,
        }
    }
}

/// How much the predicted directions agree with the labelled ones, summed
/// over the pixels which had one.
///
/// The length of either vector says nothing about its direction, so both are
/// normalized away: what is summed is the cosine of the angle between them,
/// one where they point the same way and minus one where they are opposed. A
/// pixel whose label is the zero vector — the frame, a symbol with no pattern
/// to turn — has no direction to agree with, and `has_angle` is the nought it
/// is multiplied by: the caller worked out which those were to mask the loss
/// with, and the same mask serves here rather than being found twice.
fn agreement<B: Backend>(
    predicted: Tensor<B, 2>,
    wanted: Tensor<B, 2>,
    has_angle: Tensor<B, 1>,
) -> Tensor<B, 1> {
    // A prediction shorter than this has no direction worth measuring, and
    // dividing by its length would be dividing by nothing. Clamping the
    // divisor leaves such a pixel with an agreement near zero, which is the
    // honest score for a vector pointing nowhere.
    const TINY: f32 = 1e-6;

    let length = |v: Tensor<B, 2>| v.powi_scalar(2).sum_dim(1).sqrt();
    let (predicted_length, wanted_length) = (length(predicted.clone()), length(wanted.clone()));

    let cosine = predicted.mul(wanted).sum_dim(1).div(
        predicted_length
            .clamp_min(TINY)
            .mul(wanted_length.clamp_min(TINY)),
    );

    let [pixels, _] = cosine.dims();
    cosine.reshape([pixels]).mul(has_angle).sum()
}

impl<B: AutodiffBackend> TrainStep<MapBatch<B>, MapOutput<B>> for UNet<B> {
    fn step(&self, batch: MapBatch<B>) -> TrainOutput<MapOutput<B>> {
        let output = self.forward_step(batch);
        TrainOutput::new(self, output.loss.backward(), output)
    }
}

impl<B: Backend> ValidStep<MapBatch<B>, MapOutput<B>> for UNet<B> {
    fn step(&self, batch: MapBatch<B>) -> MapOutput<B> {
        self.forward_step(batch)
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
    /// The rate the warmup climbs to and the annealing comes down from --
    /// see [`WarmupCosine`]. Not the rate of any particular step.
    #[config(default = "DEFAULT_LEARNING_RATE")]
    pub learning_rate: f64,
    /// The feature maps at full resolution — see
    /// [`crate::net::unet::UNetConfig::base_channels`].
    #[config(default = "DEFAULT_BASE_CHANNELS")]
    pub base_channels: usize,
    /// How many pixels square a crop is. Must divide by `2^DEPTH`.
    #[config(default = "CROP")]
    pub crop: usize,
    /// How many crops one pass takes from each map.
    #[config(default = "DEFAULT_CROPS_PER_MAP")]
    pub crops_per_map: usize,
    /// How far a crop may hang off the edge of a map, padded with the white
    /// a map is printed on — see [`MapDataset::with_overhang`]. `None` is
    /// half the crop, which makes every pixel of the map equally likely to be
    /// trained on; nought keeps every crop wholly inside, which under-shows
    /// whatever lives at a map's edge. [`train`] resolves it before it writes
    /// the configuration out, so a run's `training.json` holds the number it
    /// used rather than the absence of one.
    #[config(default = "None")]
    pub overhang: Option<usize>,
    /// The share of the maps kept for training, the rest for validation.
    #[config(default = 0.8)]
    pub train_share: f64,
    /// What the angle term counts for beside the class term. One is a
    /// starting point rather than a finding: the two are measured in nothing
    /// alike, and which matters more is a question about the model being
    /// built rather than about the data.
    #[config(default = "DEFAULT_ANGLE_WEIGHT")]
    pub angle_weight: f64,
    /// How many of the validation split's pictures to read back into maps
    /// each epoch, for the sheets under `image_valid/`. Nought draws none.
    #[config(default = "DEFAULT_IMAGE_VALID")]
    pub image_valid: usize,
    /// How hard the weights are pulled back towards nothing at every step —
    /// see [`DEFAULT_WEIGHT_DECAY`], which is mostly about what a batch
    /// normalization does to the scale of the layer in front of it. Nought
    /// turns it off.
    #[config(default = "DEFAULT_WEIGHT_DECAY")]
    pub weight_decay: f64,
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

/// How the moment a run was started is written into its folder's name. Fixed
/// width and biggest unit first, so that a listing of the training folder is
/// in the order the runs happened.
const RUN_STAMP: &str = "%Y_%m_%d__%H_%M_%S";

/// What burn calls a per-epoch metric folder, before [`number_epochs`] gives
/// it its number.
const BURN_EPOCH_PREFIX: &str = "epoch-";

/// Which metric decides the epoch `best.mpk` is taken from: the validation
/// loss, which is the one figure the whole of a run is descending.
const BEST_METRIC: &str = "Loss";

/// How many epochs keep their checkpoint folder.
///
/// A checkpoint is the weights, the optimizer's state and the scheduler's —
/// what it would take to carry the run on from that epoch — and on a
/// full-size model that is not small. Keeping every epoch's would multiply
/// the cost of a run by its length for the sake of epochs nobody goes back
/// to, so what is kept is the last few, which is what resuming needs.
///
/// The epoch which validated best is kept too, however far back it was: see
/// [`MetricCheckpointingStrategy`] where the two are composed. The metric
/// logs under `train/` and `valid/` are untouched by this — every epoch keeps
/// its numbers, which are a few bytes each.
pub const KEPT_CHECKPOINTS: usize = 5;

/// The weights of the epoch which validated best, at the top of the run
/// folder.
///
/// A file rather than a folder because [`DefaultRecorder`] writes a whole
/// model as one; a recorder which needed more than one would want a folder of
/// this name instead, which is why the name carries no extension of its own
/// until it is saved.
///
/// [`DefaultRecorder`] and not burn's `CompactRecorder`, which is the same
/// named-messagepack file at **half** precision. A weight fits in an `f16`
/// and so does a batch normalization's running mean; its running **variance**
/// does not. This network's reach 1e5 by the fourth level, `f16` tops out at
/// 65504, and everything past that is written down as an infinity — which
/// reads back as a channel whose output is its own bias and nothing else.
/// Nothing goes wrong while the run is going, since training normalizes by
/// the batch it is holding and the model in memory is `f32` throughout; what
/// breaks is every use of the file afterwards. See the module documentation
/// of [`crate::net::predict`].
const BEST_WEIGHTS: &str = "best";

/// The phase a run shows its work with, built out of what its folder now
/// holds: the symbol set the dataset was drawn with, and the `classes.json`
/// which says how much ground a pixel of a picture covers.
///
/// Reading those back rather than being handed them keeps the sheets and the
/// `image_to_map` tool looking at the same two files, so a sheet is a preview
/// of that tool rather than a second opinion.
fn image_validation<B: AutodiffBackend>(
    run: &Path,
    pictures: Vec<PathBuf>,
    config: &TrainingConfig,
    device: &B::Device,
) -> Result<ImageValidation<B::InnerBackend>, String> {
    // Nothing to draw, so nothing to read: a run asked for no sheets should
    // not fail over a symbol set it will never open.
    if pictures.is_empty() {
        return Ok(ImageValidation::new(
            run,
            Vec::new(),
            Path::new(""),
            Vec::new(),
            ReadBackSettings::of(config.crop, 1.0, 1),
            device.clone(),
        ));
    }

    let notes_file = run.join(CLASSES_FILE);
    let notes = Classes::read(&notes_file)?;
    let symbol_set = run.join(notes.symbol_set.ok_or_else(|| {
        format!(
            "{} names no symbol set, so there is no telling which symbol a class is",
            notes_file.display(),
        )
    })?);
    let (mut map, _) = read_xml_map(&symbol_set)
        .map_err(|e| format!("cannot read the symbol set {}: {e}", symbol_set.display()))?;
    map.resolve_references();
    let catalogue = Catalogue::of(&map);

    Ok(ImageValidation::new(
        run,
        pictures,
        &symbol_set,
        catalogue.opaque_areas,
        ReadBackSettings::of(config.crop, notes.resolution, map.scale_denominator),
        device.clone(),
    ))
}

/// Copies what the dataset says about its own answers into the run folder:
/// `classes.json`, and the symbol set it names.
///
/// What comes out of a trained network is a class index per pixel, and an
/// index means nothing on its own — which symbol it is, and how much ground
/// a pixel covers, are in those two files and nowhere in the weights. A run
/// folder which carries them is enough on its own to turn a picture into a
/// map; one which does not is a model whose answers cannot be read, however
/// well it learned them.
///
/// Copied rather than referred to, because a dataset is a temporary thing —
/// regenerated with other settings, moved, deleted — and a run outlives it.
pub fn copy_dataset_notes(dataset: &Path, run: &Path) -> Result<(), String> {
    let classes_file = dataset.join(CLASSES_FILE);
    let classes = Classes::read(&classes_file)?;
    let copy = |from: &Path, name: &str| {
        std::fs::copy(from, run.join(name))
            .map(|_| ())
            .map_err(|e| format!("cannot copy {} into {}: {e}", from.display(), run.display()))
    };
    copy(&classes_file, CLASSES_FILE)?;

    let symbol_set = classes.symbol_set.ok_or_else(|| {
        format!(
            "{} names no symbol set. A dataset carries the set its maps were drawn with, since a              class of a label is a place in that set's opaque areas and nothing else says which              place; this one was generated before that was so. Generating it again is what puts              the set there.",
            classes_file.display(),
        )
    })?;
    copy(&dataset.join(&symbol_set), &symbol_set)
}

/// Makes the folder this run writes into: `<Model>_YYYY_MM_DD__hh_mm_ss`
/// under `into`, and `into` itself if it is not there.
///
/// The name is the model's Rust type and the moment the run started, so that
/// a training folder is a history rather than one run overwriting the last.
fn run_directory(into: &Path, model: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(into).map_err(|e| format!("cannot make {}: {e}", into.display()))?;

    let base = format!("{model}_{}", chrono::Local::now().format(RUN_STAMP));
    // Two runs started inside the same second would otherwise write into one
    // another's folder, and the second of them would read as the first
    // resumed.
    let mut attempt = 1;
    loop {
        let run = into.join(if attempt == 1 {
            base.clone()
        } else {
            format!("{base}__{attempt}")
        });
        match std::fs::create_dir(&run) {
            Ok(()) => return Ok(run),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            Err(e) => return Err(format!("cannot make {}: {e}", run.display())),
        }
    }
}

/// Renames burn's `epoch-3` to `003`, `width` digits wide.
///
/// Zero padded to the widest epoch number the run can reach, so that the
/// epochs of a run list in the order they happened rather than in the order
/// their names sort — `epoch-10` comes before `epoch-2` everywhere a name is
/// sorted as text, which is a file browser, a shell glob and `ls` alike.
fn number_epochs(directory: &Path, width: usize) -> Result<(), String> {
    let read = |e| format!("cannot read {}: {e}", directory.display());
    for entry in std::fs::read_dir(directory).map_err(read)? {
        let entry = entry.map_err(read)?;
        let name = entry.file_name();
        let Some(epoch) = name
            .to_str()
            .and_then(|name| name.strip_prefix(BURN_EPOCH_PREFIX))
            .and_then(|epoch| epoch.parse::<usize>().ok())
        else {
            continue;
        };

        let to = directory.join(format!("{epoch:0width$}"));
        std::fs::rename(entry.path(), &to).map_err(|e| {
            format!(
                "cannot rename {} to {}: {e}",
                entry.path().display(),
                to.display(),
            )
        })?;
    }
    Ok(())
}

/// Moves burn's `checkpoint/model-3.mpk` into `checkpoint/003/model.mpk`, the
/// epoch numbers padded as [`number_epochs`] pads them.
///
/// What an epoch left behind is the weights, the optimizer's state and the
/// scheduler's, and those three are one thing: what it would take to carry
/// the run on from there. Written flat they are three files among every other
/// epoch's, and reading one epoch's state off a folder of thirty is arithmetic
/// on file names.
fn group_checkpoints(directory: &Path, width: usize) -> Result<(), String> {
    let read = |e| format!("cannot read {}: {e}", directory.display());
    for entry in std::fs::read_dir(directory).map_err(read)? {
        let entry = entry.map_err(read)?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // `model-3.mpk` is the model of epoch three; the recorder chose the
        // extension, so it is carried over rather than assumed.
        let Some((name, epoch)) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.rsplit_once('-'))
        else {
            continue;
        };
        let Ok(epoch) = epoch.parse::<usize>() else {
            continue;
        };

        let into = directory.join(format!("{epoch:0width$}"));
        std::fs::create_dir_all(&into)
            .map_err(|e| format!("cannot make {}: {e}", into.display()))?;

        let mut file = std::ffi::OsString::from(name);
        if let Some(extension) = path.extension() {
            file.push(".");
            file.push(extension);
        }
        let to = into.join(file);
        std::fs::rename(&path, &to)
            .map_err(|e| format!("cannot move {} to {}: {e}", path.display(), to.display(),))?;
    }
    Ok(())
}

/// Which epoch validated best, out of the metric logs the run just wrote.
///
/// Read back through burn's own summary rather than kept as the run went: an
/// epoch is scored by the mean over its batches, and that mean is what the
/// logs hold and what the table printed at the end of a run reports. So this
/// picks the epoch that table names, and not some other reading of the same
/// numbers.
///
/// Must be called before the epoch folders are renumbered — burn reads its own
/// `epoch-N` names back.
fn best_epoch(run: &Path) -> Option<usize> {
    let summary = LearnerSummary::new(run, &[BEST_METRIC]).ok()?;
    let loss = summary
        .metrics
        .valid
        .iter()
        .find(|metric| metric.name == BEST_METRIC)?;
    loss.entries
        .iter()
        // A loss which is not a number is a run which came apart, and it is
        // not the best of anything.
        .filter(|entry| !entry.value.is_nan())
        .min_by(|a, b| a.value.total_cmp(&b.value))
        .map(|entry| entry.step)
}

/// Trains a U-Net on the dataset in `dataset`, writing the run into a folder
/// of its own under `into`, and returning that folder.
///
/// `into` is the training folder — a history of runs rather than one run —
/// and is created if it is not there. Each run gets
/// `<Model>_YYYY_MM_DD__hh_mm_ss` under it, holding:
///
/// * `training.json`, the configuration it was started with, and
///   `architecture.txt`, the network that configuration built;
/// * `checkpoint/001/`, `002/`, …, a folder per checkpointed epoch, holding
///   the weights, the optimizer and the scheduler as that epoch left them.
///   Not every epoch: see [`KEPT_CHECKPOINTS`];
/// * `train/001/` and `valid/001/`, the metrics of **every** epoch on each
///   split;
/// * `experiment.log`, what the run logged as it went;
/// * `best.mpk`, the weights of the epoch which validated best;
/// * `classes.json` and the symbol set, copied out of the dataset — see
///   [`copy_dataset_notes`].
///
/// `dashboard` switches the plain, scrolling metrics log for a live
/// terminal dashboard; it says nothing about the run itself, which is why
/// it is a parameter here rather than a field of [`TrainingConfig`], the
/// part of a run's setup that gets written to disk.
pub fn train<B: AutodiffBackend>(
    dataset: &Path,
    into: &Path,
    config: TrainingConfig,
    device: B::Device,
    dashboard: bool,
) -> Result<PathBuf, String> {
    if !config.crop.is_multiple_of(1 << DEPTH) {
        return Err(format!(
            "a crop of {} pixels does not divide by {}, which is what a U-Net of {DEPTH} levels \
             halves and doubles",
            config.crop,
            1 << DEPTH,
        ));
    }

    // Half the crop unless the run asked for another figure, settled here so
    // that the configuration written below is the one the run went by.
    let mut config = config;
    config.overhang = Some(config.overhang.unwrap_or(config.crop / 2));

    let run = run_directory(into, extract_type_name::<UNet<B>>())?;
    config
        .save(run.join("training.json"))
        .map_err(|e| format!("cannot write the configuration: {e}"))?;
    copy_dataset_notes(dataset, &run)?;

    let all = MapDataset::load(dataset, config.seed)?
        .with_crop(config.crop)?
        .with_crops_per_map(config.crops_per_map)
        .with_overhang(config.overhang.unwrap_or(config.crop / 2));

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
    // Taken before the split is handed to a loader, which consumes it. The
    // validation half, because a sheet of a map the network trained on shows
    // how well it remembers rather than how well it reads.
    let pictures: Vec<PathBuf> = valid
        .pictures()
        .take(config.image_valid)
        .map(Path::to_path_buf)
        .collect();
    println!(
        "{}: {maps} maps, {} to train on and {} to validate against, {} crops of {} pixels from \
         each of them each pass",
        dataset.display(),
        train.maps(),
        valid.maps(),
        config.crops_per_map,
        config.crop,
    );
    println!("{}: the run", run.display());

    // Counted before the model is built, since the loss inside it carries
    // the weights. A sample of the training half only: the validation half is
    // scored on the same weights, but counting it in would be reading the
    // labels the run is meant to be held out from.
    let weights = class_weights(&train)?;
    println!(
        "the classes are weighted {}, the frame last",
        weights
            .iter()
            .map(|weight| format!("{weight:.2}"))
            .collect::<Vec<_>>()
            .join(", "),
    );

    // Built here rather than inside the builder so that what the run trained
    // can be written down before it starts training it.
    let model = UNetConfig::new(config.classes)
        .with_base_channels(config.base_channels)
        .with_angle_weight(config.angle_weight)
        .with_class_weights(Some(weights))
        .init::<B>(&device);
    let architecture = run.join("architecture.txt");
    std::fs::write(&architecture, format!("{model}\n"))
        .map_err(|e| format!("cannot write {}: {e}", architecture.display()))?;

    // How many steps one pass over the training half takes, which is what
    // the learning rate is annealed against. Taken before the loader is
    // built, since building it consumes the dataset. The last batch of an
    // epoch is a short one wherever the split does not divide, and burn runs
    // it rather than dropping it, so this rounds up.
    let train_batches = train.len().div_ceil(config.batch_size).max(1);

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

    // What the run shows its work on. Built twice — once for the hook, once
    // for the catch-up after `fit` — because the learner takes ownership of
    // the first.
    let phase = image_validation::<B>(&run, pictures.clone(), &config, &device)?;
    if phase.wanted() {
        println!(
            "{}/: {} pictures read back into maps each epoch, drawn beside the originals",
            run.join(IMAGE_VALID).display(),
            pictures.len(),
        );
    }

    let mut builder = LearnerBuilder::new(&run)
        .metric_train_numeric(LossMetric::<MetricBackend>::new())
        .metric_valid_numeric(LossMetric::<MetricBackend>::new())
        .metric_train_numeric(PixelAccuracyMetric::<MetricBackend>::new())
        .metric_valid_numeric(PixelAccuracyMetric::<MetricBackend>::new())
        .metric_train_numeric(AngleMetric::<MetricBackend>::new())
        .metric_valid_numeric(AngleMetric::<MetricBackend>::new())
        .with_file_checkpointer(DefaultRecorder::new())
        // burn's own default, with a longer tail: the last few epochs, so
        // that a run can be carried on from where it stopped, and the epoch
        // which validated best however far back it was, so that `best.mpk`
        // has something to be copied from. Composed rather than chosen
        // between — a composition deletes an epoch only when every strategy
        // in it has finished with that epoch.
        .with_checkpointing_strategy(
            ComposedCheckpointingStrategy::builder()
                .add(KeepLastNCheckpoints::new(KEPT_CHECKPOINTS))
                .add(MetricCheckpointingStrategy::new(
                    &LossMetric::<MetricBackend>::new(),
                    Aggregate::Mean,
                    Direction::Lowest,
                    Split::Valid,
                ))
                .build(),
        )
        .devices(vec![device.clone()])
        .num_epochs(config.epochs)
        // Not a stopping strategy: the one hook burn calls once an epoch. It
        // always answers that the run should carry on — see
        // `crate::net::image_valid`.
        .early_stopping(phase)
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
    // How many steps the run is, which is what the annealing is spread over:
    // one step per batch, every batch of every epoch.
    let steps = config.epochs * train_batches;
    let learner = builder.build(
        model,
        // Clipped rather than bare. Adam scales a step by the gradient's own
        // recent size and so survives a large one, but not the step after a
        // gradient hundreds of times the usual: that one moves the weights
        // far enough that the batch normalization's running statistics are
        // left describing a network which no longer exists, and a validation
        // pass through the stale ones comes back as a loss in the billions.
        // The norm is the ceiling on how far one batch can move the run.
        // AdamW rather than Adam: the decay has to be decoupled from the
        // gradient to hold a scale the loss is indifferent to -- see
        // [`DEFAULT_WEIGHT_DECAY`]. A decay of nought makes the two the same
        // optimizer.
        AdamWConfig::new()
            .with_weight_decay(config.weight_decay as f32)
            .with_grad_clipping(Some(GradientClippingConfig::Norm(GRADIENT_NORM)))
            .init(),
        WarmupCosine::new(config.learning_rate, steps),
    );

    let trained = learner.fit(train_loader, valid_loader);

    // While the folders are still the `epoch-N` burn reads back, and before
    // they are renumbered under it.
    let best = best_epoch(&run);

    // The hook draws an epoch behind, so the last one is always outstanding
    // here; and a run which was interrupted may be missing more. Every
    // checkpoint which survived gets its sheets now that the checkpointer's
    // thread has been joined and its writes are all on disk.
    let phase = image_validation::<B>(&run, pictures, &config, &device)?;
    if let Err(message) = phase.catch_up() {
        eprintln!("Warning: cannot draw the last epochs: {message}");
    }

    // Wide enough for the last epoch the run was set up for, rather than for
    // the last it reached: the width is a property of the run, and an
    // interrupted one should number its epochs the way a finished one would.
    let width = config.epochs.to_string().len();
    group_checkpoints(&run.join("checkpoint"), width)?;
    for split in ["train", "valid", IMAGE_VALID] {
        let folder = run.join(split);
        if folder.is_dir() {
            number_epochs(&folder, width)?;
        }
    }

    let weights = run.join(BEST_WEIGHTS).with_extension("mpk");
    let kept = best
        .map(|epoch| {
            let from = run
                .join("checkpoint")
                .join(format!("{epoch:0width$}"))
                .join("model.mpk");
            (epoch, from)
        })
        .filter(|(_, from)| from.is_file());

    match kept {
        // Copied out of the epoch's own folder rather than saved from memory:
        // what `fit` returns is the last epoch, and the last epoch is only the
        // best one when the run was still improving when it stopped.
        Some((epoch, from)) => {
            std::fs::copy(&from, &weights).map_err(|e| {
                format!(
                    "cannot copy {} to {}: {e}",
                    from.display(),
                    weights.display()
                )
            })?;
            println!(
                "{}: the weights of epoch {epoch}, which validated best",
                weights.display(),
            );
        }
        // No validation loss to read — a run of no epochs, or logs which could
        // not be read back — or its epoch has no checkpoint left. The model in
        // hand is the last epoch, and it is then the only answer there is.
        None => {
            trained
                .save_file(run.join(BEST_WEIGHTS), &DefaultRecorder::new())
                .map_err(|e| format!("cannot write the weights: {e}"))?;
            println!(
                "{}: the weights of the last epoch, there being no best epoch's checkpoint to \
                 take them from",
                weights.display(),
            );
        }
    }

    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder holding empty files at the given names.
    fn folder(names: &[&str]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a temporary folder");
        for name in names {
            std::fs::write(directory.path().join(name), []).expect("a file");
        }
        directory
    }

    /// What a folder holds, sorted, folders' contents written as `dir/file`.
    fn listing(directory: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(directory).expect("a folder") {
            let entry = entry.expect("an entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                let inside = listing(&entry.path());
                if inside.is_empty() {
                    found.push(name);
                } else {
                    found.extend(inside.into_iter().map(|inner| format!("{name}/{inner}")));
                }
            } else {
                found.push(name);
            }
        }
        found.sort();
        found
    }

    #[test]
    fn an_epoch_is_numbered_to_the_width_of_the_last_one() {
        let directory = tempfile::tempdir().expect("a temporary folder");
        for epoch in [1, 2, 10, 100] {
            std::fs::create_dir(directory.path().join(format!("epoch-{epoch}"))).expect("an epoch");
        }
        std::fs::write(directory.path().join("epoch-1/Loss.log"), b"1.0").expect("a metric");

        number_epochs(directory.path(), 3).expect("the epochs renumbered");
        assert_eq!(
            listing(directory.path()),
            ["001/Loss.log", "002", "010", "100"],
        );
    }

    /// A run folder can hold things which are not epochs, and renaming those
    /// would be renaming a file the tool does not own.
    #[test]
    fn what_is_not_an_epoch_is_left_where_it_is() {
        let directory = folder(&["epoch-none", "notes.txt"]);
        std::fs::create_dir(directory.path().join("epoch-2")).expect("an epoch");

        number_epochs(directory.path(), 2).expect("the epochs renumbered");
        assert_eq!(listing(directory.path()), ["02", "epoch-none", "notes.txt"]);
    }

    #[test]
    fn an_epochs_checkpoint_files_are_gathered_into_its_own_folder() {
        let directory = folder(&[
            "model-1.mpk",
            "optim-1.mpk",
            "scheduler-1.mpk",
            "model-12.mpk",
        ]);

        group_checkpoints(directory.path(), 2).expect("the checkpoints grouped");
        assert_eq!(
            listing(directory.path()),
            [
                "01/model.mpk",
                "01/optim.mpk",
                "01/scheduler.mpk",
                "12/model.mpk",
            ],
        );
    }

    /// The epoch is what follows the *last* dash, and the extension is the
    /// recorder's rather than this module's to choose.
    #[test]
    fn a_checkpoint_keeps_its_name_and_its_extension() {
        let directory = folder(&["model-ema-3.bin", "model-3", "loose.txt"]);

        group_checkpoints(directory.path(), 3).expect("the checkpoints grouped");
        assert_eq!(
            listing(directory.path()),
            ["003/model", "003/model-ema.bin", "loose.txt"],
        );
    }

    /// The whole point of the warmup: the first step is a small fraction of
    /// the peak rather than the peak itself, and the climb is even.
    #[test]
    fn the_rate_climbs_to_the_peak_and_then_comes_down() {
        let peak = 3.0e-4;
        let total = 10_000;
        let mut schedule = WarmupCosine::new(peak, total);

        let first = schedule.step();
        assert!(first > 0.0, "a first step of nothing teaches nothing");
        assert!(
            first < peak / 100.0,
            "the first step is {first}, not a warmup"
        );

        // Up to the peak, and no further: the last step of the warmup is the
        // largest rate the run ever takes.
        let mut largest: f64 = first;
        for _ in 1..WARMUP_STEPS {
            largest = largest.max(schedule.step());
        }
        assert!(
            (largest - peak).abs() < 1e-12,
            "climbed to {largest}, not {peak}"
        );

        // And down from there, never back above it.
        let mut last = largest;
        for _ in WARMUP_STEPS..total {
            let rate = schedule.step();
            assert!(
                rate <= last + 1e-12,
                "the rate went back up: {last} then {rate}"
            );
            last = rate;
        }
        let wanted = peak * FINAL_SHARE;
        assert!(
            (last - wanted).abs() < peak * 1e-3,
            "ended at {last}, not near {wanted}",
        );
    }

    /// A run of fewer steps than the warmup is a run which would otherwise
    /// never reach its rate at all.
    #[test]
    fn a_short_run_is_not_all_warmup() {
        let peak = 1.0e-3;
        let mut schedule = WarmupCosine::new(peak, 10);
        let rates: Vec<f64> = (0..10).map(|_| schedule.step()).collect();
        let largest = rates.iter().cloned().fold(f64::MIN, f64::max);
        assert!(
            (largest - peak).abs() < 1e-12,
            "a ten step run peaked at {largest}, not {peak}",
        );
    }

    /// A checkpoint carries the schedule as well as the weights, so a run
    /// carried on from one carries on at the rate it left off at rather than
    /// warming up all over again.
    #[test]
    fn the_schedule_comes_back_where_it_was_left() {
        let mut schedule = WarmupCosine::new(3.0e-4, 1_000);
        for _ in 0..250 {
            schedule.step();
        }
        let record = schedule.to_record::<MetricBackend>();
        let wanted = schedule.step();

        let loaded = WarmupCosine::new(3.0e-4, 1_000).load_record::<MetricBackend>(record);
        let mut loaded = loaded;
        assert_eq!(loaded.step(), wanted);
    }

    /// The weighting is inverse to the share, so the rare class is worth more
    /// than the common one -- which is the whole of what it is for.
    #[test]
    fn a_rare_class_is_weighted_above_a_common_one() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("dataset");
        let settings = crate::dataset::Settings {
            layout_size: 2,
            cell_size: 20,
            maps: 3,
            just_opaque_areas: true,
            resolution: 2.0,
            frame: 5.0,
            ..crate::dataset::Settings::default()
        };
        crate::dataset::create_dataset(
            Path::new("tests/data/turning_patterns.xmap"),
            &folder,
            &settings,
            |_, _| {},
        )
        .expect("the dataset generates");

        let dataset = MapDataset::load(&folder, 0).expect("the dataset loads");
        let shares = dataset.class_balance(BALANCE_SAMPLE).expect("a balance");
        let weights = class_weights(&dataset).expect("the weights are counted");

        assert_eq!(weights.len(), shares.len());
        assert!(weights.iter().all(|&weight| weight > 0.0), "{weights:?}");

        // Whichever two classes those turned out to be: the larger share is
        // the smaller weight.
        let commonest = shares
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("a class")
            .0;
        let rarest = shares
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .expect("a class")
            .0;
        assert!(
            weights[rarest] >= weights[commonest],
            "the rarest class weighs {} and the commonest {}",
            weights[rarest],
            weights[commonest],
        );

        // Scaled so that the average would be one, and then floored, so it
        // comes out at one or a little above -- never below, which would be a
        // loss quietly turned down.
        let mean = weights.iter().map(|&w| w as f64).sum::<f64>() / weights.len() as f64;
        assert!(mean >= 1.0 - 1e-5, "the weights average {mean}");
    }

    /// A class is weighted up for being rare and never down for being common.
    ///
    /// Inverting a share lifts the rare classes by pushing the common ones
    /// below one, and the commonest class of a generated map is the white
    /// frame at a third of every picture. Left below one it becomes the
    /// cheapest thing in the dataset to get wrong — which is how a run comes
    /// to fill the border with ground cover. See [`class_weights`].
    #[test]
    fn no_class_is_weighted_below_one_for_being_common() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("dataset");
        let settings = crate::dataset::Settings {
            layout_size: 2,
            cell_size: 20,
            maps: 3,
            just_opaque_areas: true,
            resolution: 2.0,
            frame: 5.0,
            ..crate::dataset::Settings::default()
        };
        crate::dataset::create_dataset(
            Path::new("tests/data/turning_patterns.xmap"),
            &folder,
            &settings,
            |_, _| {},
        )
        .expect("the dataset generates");

        let dataset = MapDataset::load(&folder, 0).expect("the dataset loads");
        let shares = dataset.class_balance(BALANCE_SAMPLE).expect("a balance");
        let weights = class_weights(&dataset).expect("the weights are counted");

        // The frame is the last class and the commonest thing on any of these
        // maps, and it is not weighted down for it.
        let frame = weights.len() - 1;
        assert!(
            shares[frame] > *shares[..frame].iter().max_by(|a, b| a.total_cmp(b)).expect("a class"),
            "the frame should be the commonest class here: {shares:?}",
        );
        assert!(
            weights.iter().all(|&weight| weight >= 1.0),
            "{weights:?} holds a class weighted below one",
        );
    }

    /// A crop with no rotatable symbol in it has nothing to say about
    /// angles, and the angle term says nothing about it: the loss is the
    /// class term alone. Unmasked, the same crop would have been thousands of
    /// pixels of "point nowhere" -- which is what a network trained on it
    /// learned to do everywhere.
    #[test]
    fn a_crop_with_no_angle_in_it_is_scored_on_its_classes_alone() {
        use burn::backend::ndarray::{NdArray, NdArrayDevice};
        type TestBackend = NdArray<f32>;

        let device = NdArrayDevice::default();
        let classes = 3;
        // Weighted heavily towards the angle, so that a term which was still
        // being taken over the unangled pixels could not hide in the rounding.
        let net = UNetConfig::new(classes)
            .with_base_channels(4)
            .with_angle_weight(100.0)
            .init::<TestBackend>(&device);

        let image = Tensor::<TestBackend, 4>::zeros([1, 3, 16, 16], &device);
        let class = Tensor::<TestBackend, 3, burn::tensor::Int>::zeros([1, 16, 16], &device);

        // Every label the zero vector: no pixel of this crop has an angle.
        let none = MapBatch {
            image: image.clone(),
            class: class.clone(),
            angle: Tensor::zeros([1, 2, 16, 16], &device),
        };
        let without = net.forward_step(none);
        let angled: f32 = without.angled.clone().into_scalar();
        assert_eq!(angled, 0.0, "no pixel of that crop had an angle");

        // The same crop scored on its classes and nothing else.
        let plain = net.class_loss().forward(
            net.forward(image.clone())
                .permute([0, 2, 3, 1])
                .reshape([16 * 16, classes + 3])
                .slice([0..16 * 16, 0..classes + 1]),
            class.clone().reshape([16 * 16]),
        );
        let (whole, expected): (f32, f32) = (without.loss.into_scalar(), plain.into_scalar());
        assert!(
            (whole - expected).abs() < 1e-6,
            "the loss was {whole} and the class term alone {expected}",
        );

        // And a crop which does have angles is scored on more than that.
        let some = MapBatch {
            image,
            class,
            angle: Tensor::ones([1, 2, 16, 16], &device),
        };
        let with = net.forward_step(some);
        let angled: f32 = with.angled.into_scalar();
        assert_eq!(angled, 16.0 * 16.0, "every pixel of that crop had one");
        let whole: f32 = with.loss.into_scalar();
        assert!(
            whole > expected + 1e-6,
            "an angled crop scored {whole}, no more than the {expected} of an unangled one",
        );
    }

    /// A class the sample never saw would be one over nothing. It is held to
    /// the floor instead, which is a large weight rather than an infinite
    /// one.
    #[test]
    fn a_class_which_was_never_seen_does_not_take_the_whole_loss() {
        // Not through the dataset: what is being checked is the arithmetic on
        // a balance with a nought in it, and a generated dataset draws every
        // symbol it was given.
        let shares = [0.5, 0.5, 0.0];
        let inverted: Vec<f64> = shares
            .iter()
            .map(|&share: &f64| 1.0 / share.max(RAREST_SHARE))
            .collect();
        assert!(
            inverted.iter().all(|weight| weight.is_finite()),
            "{inverted:?}"
        );
        assert_eq!(inverted[2], 1.0 / RAREST_SHARE);
    }
}
