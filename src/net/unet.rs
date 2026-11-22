//! The U-Net, and what its channels mean.
//!
//! A rendered map is read back one pixel at a time: for each pixel of the
//! image, which of the symbol set's opaque areas was drawn there, and at what
//! angle its fill pattern was turned to. That is a segmentation, so the shape
//! of the network is the shape every segmentation network has been since
//! 2015 — an encoder which halves the picture four times over while it works
//! out *what* is in it, a decoder which doubles it back to full size, and a
//! skip connection across each level, because by the time the encoder knows
//! the cell is bare rock it has thrown away which pixel the boundary ran
//! through, and the skip is where that comes back.
//!
//! # What comes out
//!
//! [`crate::net::data`] hands the label over as `on + 2` channels — a one-hot
//! over the `on` opaque areas, all zeros for the white frame, then the sine and
//! the cosine of the angle. The head does **not** emit those `on + 2`
//! numbers directly. It emits `on + 3`:
//!
//! * `on + 1` **class logits**, one per opaque area and one more for the
//!   frame. A pixel is exactly one of those and never two, so they belong
//!   under one softmax and one cross-entropy — and the frame has to be
//!   somewhere in that softmax, since "none of them" is a thing the picture
//!   really says. It has no channel in the label because all zeros already
//!   means it; it needs one here because a logit of "no class" is not the
//!   absence of a logit.
//! * 2 for the angle, straight out, no activation: the sine and the cosine,
//!   scored against the label's own pair. The zero vector is the target
//!   where there is no angle to give — the frame, and a symbol with no
//!   pattern to turn — so the head has to learn to shrink to nothing there,
//!   which it can, and which a unit-normalized output could not.
//!
//! After them come the layers drawn over the ground, where the network has
//! them: a logit per line class (see [`crate::line_truth`]), then a logit
//! per point class, a heatmap which peaks where a point of that class is
//! (see [`crate::point_truth`]). Both are a sigmoid each, not a softmax. A
//! network built with [`UNetConfig::point_angles`] has two channels more
//! after those, the sine and the cosine of the angle each point is turned to,
//! straight out with no activation, as the ground's two are.
//!
//! [`UNet::forward`] is those `on + 3` raw channels and the layers' after
//! them, which is what the loss wants. [`UNet::predict`] is the answer in the dataset's own shape: the
//! softmax over the `on + 1`, with the frame channel dropped, and the two
//! angle channels after it — `on + 2`, matching a `gt/*.bin` expanded by
//! [`crate::ground_truth::GroundTruth::one_hot`] channel for channel.
//!
//! # How dense the picture is
//!
//! A picture says nothing about how many pixels a meter of ground is, and the
//! map read out of it is only the right size if that is known. A network
//! built with [`UNetConfig::scale_head`] answers it as well: a small head on
//! the bottom of the encoder — a global average over the picture, then two
//! dense layers — which says how much denser than the density its dataset
//! was normally drawn at the picture is, as a natural logarithm. It is
//! trained on a dataset drawn over a range of densities (see
//! [`crate::dataset::Settings::resolution_range`]), where the symbols, whose
//! sizes are fixed on the paper, are the only thing which shows it.
//! [`UNet::forward`] leaves it out; [`UNet::forward_all`] and
//! [`UNet::scale`] are where it is.
//!
//! Everything here is `NCHW`, which is the layout burn's convolutions work
//! in: `[batch, channel, row, column]`. The label on disk is `H, W, C`, the
//! same numbers read in a different order, and [`crate::net::data`] is where
//! the order changes — once, while a batch is built, rather than on every
//! tensor that passes through here.

use std::collections::BTreeMap;

use burn::config::Config;
use burn::module::Module;
use burn::module::Param;
use burn::nn::conv::{Conv2d, Conv2dConfig, ConvTranspose2d, ConvTranspose2dConfig};
use burn::nn::loss::{CrossEntropyLoss, CrossEntropyLossConfig};
use burn::nn::pool::{MaxPool2d, MaxPool2dConfig};
use burn::nn::{BatchNorm, BatchNormConfig, Linear, LinearConfig, PaddingConfig2d, Relu};
use burn::tensor::activation::{sigmoid, softmax};
use burn::tensor::{Device, Tensor, TensorData};

use serde_json::Value;

use crate::net::describe::{halved, layer, NetDescriptor, Node};
use crate::net::training::DEFAULT_ANGLE_WEIGHT;

/// How many times the picture is halved on the way down and doubled on the
/// way back up.
///
/// Four, as the paper has it: the bottom of the net sees a sixteenth of the
/// image in each direction, which at the three pixels per meter a dataset is
/// drawn at is a patch some fifty meters across — about a third of a cell,
/// and enough of one to tell a fill pattern apart from its neighbour.
pub const DEPTH: usize = 4;

/// The default number of feature maps at full resolution, doubling at every
/// step down.
///
/// The paper's is 64, which is a network of some 31 million weights. This is
/// a first model on a dataset of a few hundred maps, so it starts a quarter
/// of the way there and can be turned up.
pub const DEFAULT_BASE_CHANNELS: usize = 16;

/// How wide the scale head's hidden layer is. One number is all it answers,
/// out of an average of the bottom's features, so it needs little.
pub const SCALE_HIDDEN: usize = 64;

/// What the scale term counts for beside the class term, by default. The
/// scale is a natural logarithm, so an answer a tenth out costs a tenth: of
/// the order a class loss is by the time the network reads the ground well.
pub const DEFAULT_SCALE_WEIGHT: f64 = 1.0;

/// What a [`UNet`] is to be built as.
#[derive(Config, Debug)]
pub struct UNetConfig {
    /// How many opaque area symbols the symbol set holds, which is how many
    /// one-hot channels a label has. The head has two more than this: the
    /// frame, and the angle.
    pub classes: usize,
    /// How many channels the input image has. Three: a rendered map is RGB.
    #[config(default = 3)]
    pub in_channels: usize,
    /// The feature maps at full resolution, doubling at every step down.
    #[config(default = "DEFAULT_BASE_CHANNELS")]
    pub base_channels: usize,
    /// What the angle term counts for beside the class term — see
    /// [`crate::net::training::TrainingConfig::angle_weight`]. Carried by the
    /// model rather than passed to the loss because burn's `TrainStep` hands
    /// a step nothing but the batch and the model, so the model is the only
    /// place a run's own figure can reach it from.
    #[config(default = "DEFAULT_ANGLE_WEIGHT")]
    pub angle_weight: f64,
    /// What each class counts for in the cross-entropy, the frame last:
    /// `classes + 1` figures, or `None` for a loss which counts every pixel
    /// the same. See [`crate::net::training::class_weights`] for where a
    /// run's come from and why it wants them.
    #[config(default = "None")]
    pub class_weights: Option<Vec<f32>>,
    /// How many line symbols the lines layer has: nought for a network which
    /// reads the ground alone, which is then exactly the network it was before
    /// there was such a layer. Each is one more channel of the head, a logit
    /// for *a line of this symbol runs through this pixel*, scored
    /// independently of the others — two lines may cross — see
    /// `crate::net::training::lines_loss`.
    #[config(default = 0)]
    pub lines: usize,
    /// What the lines term counts for beside the ground's.
    #[config(default = 1.0)]
    pub line_weight: f64,
    /// How many point symbols the points layer has: nought for a network
    /// with no such layer, which is then exactly the network it was before.
    /// Each is one more channel of the head after the lines', a logit for
    /// *a point of this symbol sits at this pixel*, trained against a
    /// heatmap — see `crate::net::training::points_loss`.
    #[config(default = 0)]
    pub points: usize,
    /// What the points term counts for beside the ground's.
    #[config(default = 1.0)]
    pub point_weight: f64,
    /// Whether the points layer also says which way each point is turned:
    /// two more channels after the heatmaps, the sine and the cosine of the
    /// angle, straight out as the ground's two are, and shared by every point
    /// symbol — see [`crate::point_truth::PointTruth::angles`]. Off is the
    /// network it was before there were such channels.
    #[config(default = false)]
    pub point_angles: bool,
    /// Whether the network also says how dense the picture is: a head on
    /// the bottom of the encoder answering `ln(density / normal)` for the
    /// whole picture — see the module documentation. Off is the network it
    /// was before there was such a head, weight for weight.
    #[config(default = false)]
    pub scale_head: bool,
    /// What the scale term counts for beside the ground's.
    #[config(default = "DEFAULT_SCALE_WEIGHT")]
    pub scale_weight: f64,
}

/// How many times what a forward pass keeps a training step holds at its
/// peak: the activations kept for the backward pass, and their gradients as
/// it makes them. A round figure, not a measurement: the allocator's slack
/// and the layers' own scratch space are in it too.
pub const ACTIVATION_FACTOR: u64 = 2;

/// How many copies of the weights a training step holds: the weights, their
/// gradients, and the optimizer's two moments.
pub const WEIGHT_COPIES: u64 = 4;

impl UNetConfig {
    /// Roughly how many bytes of device memory a training step on `batch`
    /// crops `crop` pixels square holds: the activations of the forward pass
    /// (out of [`UNetConfig::describe`], so it follows the architecture)
    /// times [`ACTIVATION_FACTOR`], the labels, and the weights times
    /// [`WEIGHT_COPIES`], all as 32-bit floats. Not the backend's own
    /// overhead, which is a fixed amount per process. `None` for a crop the
    /// network cannot take (one not divisible by `2^DEPTH`).
    pub fn training_bytes(&self, batch: usize, crop: usize) -> Option<u64> {
        if crop == 0 || !crop.is_multiple_of(1 << DEPTH) {
            return None;
        }
        let (b, c) = (i64::try_from(batch).ok()?, i64::try_from(crop).ok()?);
        let descriptor = self.describe();
        let activations = descriptor.activation_count(&[("B", b), ("H", c), ("W", c)])?;
        let labels = (batch
            * (self.classes + 2 + self.lines + self.points + self.point_angle_channels())
            * crop
            * crop) as u64;
        let floats =
            activations * ACTIVATION_FACTOR + labels + descriptor.total_params() * WEIGHT_COPIES;
        floats.checked_mul(4)
    }

    /// How many channels the head emits: the ground's, then the lines', then
    /// the points' and their angle.
    pub fn head_channels(&self) -> usize {
        self.classes + 3 + self.lines + self.points + self.point_angle_channels()
    }

    /// The two channels of the points' angle, where there are points and
    /// they have one.
    fn point_angle_channels(&self) -> usize {
        if self.points > 0 && self.point_angles {
            2
        } else {
            0
        }
    }

    /// Builds the network on `device`, with its weights initialized.
    pub fn init(&self, device: &Device) -> UNet {
        let width = |level: usize| self.base_channels << level;

        let mut down = Vec::with_capacity(DEPTH);
        let mut from = self.in_channels;
        for level in 0..DEPTH {
            down.push(DoubleConvConfig::new(from, width(level)).init(device));
            from = width(level);
        }
        // The bottom, which sees the whole of what the encoder made of the
        // picture and none of the resolution.
        let bottom = DoubleConvConfig::new(from, width(DEPTH)).init(device);

        // Up the other side, deepest first, so `up[0]` is the step which
        // takes the bottom back to the fourth level.
        let mut up = Vec::with_capacity(DEPTH);
        for level in (0..DEPTH).rev() {
            up.push(UpBlockConfig::new(width(level + 1), width(level)).init(device));
        }

        UNet {
            down,
            pool: MaxPool2dConfig::new([2, 2]).with_strides([2, 2]).init(),
            bottom,
            up,
            // One weight per feature per output channel: the head decides
            // what a pixel is out of what the last block made of it, and
            // looks at nothing around it, since the looking around is what
            // the rest of the network was for.
            head: self.head(device),
            scale: self.scale_head.then(|| ScaleHead {
                hidden: LinearConfig::new(width(DEPTH), SCALE_HIDDEN).init(device),
                relu: Relu::new(),
                out: LinearConfig::new(SCALE_HIDDEN, 1).init(device),
            }),
            scale_weight: self.scale_weight,
            classes: self.classes,
            lines: self.lines,
            line_weight: self.line_weight,
            points: self.points,
            point_weight: self.point_weight,
            point_angles: self.points > 0 && self.point_angles,
            // Built once here rather than per step: the weights are a tensor,
            // and a tensor built inside a training step is a tensor built
            // tens of thousands of times an epoch.
            class_loss: CrossEntropyLossConfig::new()
                .with_weights(self.class_weights.clone())
                .init(device),
            angle_weight: self.angle_weight,
        }
    }
}

/// What a point channel of the head starts out saying, as a logit: that a
/// point is there one time in a hundred.
///
/// A point is one pixel in tens of thousands, and a head which starts at a
/// logit of nought says a point is as likely as not at every one of them:
/// the first steps of a run are then spent on nothing but taking that back,
/// with gradients large enough to unsettle what the ground's channels are
/// learning. Starting where the answer nearly always is spares it that.
pub const POINT_PRIOR_LOGIT: f32 = -4.6;

impl UNetConfig {
    /// The 1x1 convolution which turns features into an answer, its point
    /// channels' bias set to [`POINT_PRIOR_LOGIT`].
    fn head(&self, device: &Device) -> Conv2d {
        // One weight per feature per output channel: the head decides what
        // a pixel is out of what the last block made of it, and looks at
        // nothing around it, since the looking around is what the rest of
        // the network was for.
        let mut head =
            Conv2dConfig::new([self.base_channels, self.head_channels()], [1, 1]).init(device);
        if self.points > 0 {
            if let Some(bias) = &head.bias {
                let mut values: Vec<f32> = bias
                    .val()
                    .into_data()
                    .try_to_vec()
                    .expect("a bias is floats");
                // The heatmaps alone: the angle after them starts at nought,
                // which is no angle, as the ground's does.
                let first = self.classes + 3 + self.lines;
                for value in &mut values[first..first + self.points] {
                    *value = POINT_PRIOR_LOGIT;
                }
                let channels = values.len();
                head.bias = Some(Param::from_tensor(Tensor::from_data(
                    TensorData::new(values, [channels]),
                    device,
                )));
            }
        }
        head
    }
}

/// A U-Net which reads a rendered map back into the symbols it was drawn
/// with.
#[derive(Module, Debug)]
pub struct UNet {
    /// The encoder, one block per level, full resolution first.
    down: Vec<DoubleConv>,
    /// What halves the picture between one level and the next.
    pool: MaxPool2d,
    /// The block at the bottom, between the last halving and the first
    /// doubling.
    bottom: DoubleConv,
    /// The decoder, deepest first.
    up: Vec<UpBlock>,
    /// The 1x1 convolution which turns features into an answer.
    head: Conv2d,
    /// What says how dense the picture is, where the network was built to —
    /// see [`UNetConfig::scale_head`].
    scale: Option<ScaleHead>,
    /// What the scale term counts for — see [`UNetConfig::scale_weight`].
    scale_weight: f64,
    /// How many opaque areas the symbol set holds — see [`UNetConfig`].
    classes: usize,
    /// How many line symbols the head has a channel for — see
    /// [`UNetConfig::lines`].
    lines: usize,
    /// What the lines term counts for — see [`UNetConfig::line_weight`].
    line_weight: f64,
    /// How many point symbols the head has a channel for — see
    /// [`UNetConfig::points`].
    points: usize,
    /// What the points term counts for — see [`UNetConfig::point_weight`].
    point_weight: f64,
    /// Whether the head says which way each point is turned — see
    /// [`UNetConfig::point_angles`].
    point_angles: bool,
    /// The cross-entropy the class logits are scored with, its per-class
    /// weights already in it — see [`UNetConfig::class_weights`].
    class_loss: CrossEntropyLoss,
    /// What the angle term counts for — see [`UNetConfig::angle_weight`].
    angle_weight: f64,
}

impl UNet {
    /// The raw head, `[batch, classes + 3, height, width]`: the `classes + 1`
    /// class logits, the frame's among them, then the sine and the cosine of
    /// the angle.
    ///
    /// Logits, not probabilities — the loss takes it from here, and a
    /// softmax applied twice is a softmax applied wrong. [`UNet::predict`] is
    /// the same forward pass finished off into an answer.
    ///
    /// The image is `[batch, 3, height, width]`, and its height and width
    /// must both divide by `2^DEPTH`: the encoder halves them four times and
    /// the decoder puts them back, and a size which does not divide comes
    /// back a pixel short of what the skip connection expects.
    pub fn forward(&self, image: Tensor<4>) -> Tensor<4> {
        self.forward_all(image).0
    }

    /// [`UNet::forward`], and how dense the network says each picture is:
    /// `[batch, 1]` of `ln(density / normal)`, where it has a scale head.
    pub fn forward_all(&self, image: Tensor<4>) -> (Tensor<4>, Option<Tensor<2>>) {
        self.forward_all_tapped(image, &mut |_, _| {})
    }

    /// How dense the network says each picture is, `[batch, 1]` of
    /// `ln(density / normal)`, or `None` for a network with no scale head.
    ///
    /// The encoder alone: the decoder says where things are, and nothing of
    /// that goes into the scale.
    pub fn scale(&self, image: Tensor<4>) -> Option<Tensor<2>> {
        let head = self.scale.as_ref()?;
        let tap = &mut |_: &str, _: &[usize]| {};
        let (bottom, _) = self.encode_tapped(image, tap);
        Some(head.forward_tapped(bottom, tap))
    }

    /// Whether the network says how dense a picture is.
    pub fn has_scale_head(&self) -> bool {
        self.scale.is_some()
    }

    /// What the scale term counts for beside the class term.
    pub fn scale_weight(&self) -> f64 {
        self.scale_weight
    }

    /// [`UNet::forward`], reporting the shape of every tensor it makes to
    /// `tap` under the name [`UNetConfig::describe`] gives it: what holds the
    /// descriptor to the code.
    #[cfg(test)]
    pub(crate) fn forward_tapped(
        &self,
        image: Tensor<4>,
        tap: &mut dyn FnMut(&str, &[usize]),
    ) -> Tensor<4> {
        self.forward_all_tapped(image, tap).0
    }

    /// [`UNet::forward_all`], reporting every tensor's shape to `tap`.
    pub(crate) fn forward_all_tapped(
        &self,
        image: Tensor<4>,
        tap: &mut dyn FnMut(&str, &[usize]),
    ) -> (Tensor<4>, Option<Tensor<2>>) {
        let (x, skips) = self.encode_tapped(image, tap);

        // How dense the picture is, out of what the encoder made of all of
        // it.
        let scale = self
            .scale
            .as_ref()
            .map(|head| head.forward_tapped(x.clone(), tap));

        // And up, deepest first, each step handed back the level it lost.
        tap("unet/decoder@in:x", &x.dims());
        let mut x = x;
        for (step, (block, skip)) in self.up.iter().zip(skips.into_iter().rev()).enumerate() {
            tap(
                &format!("unet/decoder@in:skip{}", DEPTH - 1 - step),
                &skip.dims(),
            );
            x = block.forward_tapped(x, skip, &format!("unet/decoder/up{step}"), tap);
        }
        tap("unet/decoder", &x.dims());

        let x = self.head.forward(x);
        tap("unet/head", &x.dims());
        (x, scale)
    }

    /// The encoder: what the bottom made of the picture, and what each level
    /// saw before it was halved, full resolution first.
    fn encode_tapped(
        &self,
        image: Tensor<4>,
        tap: &mut dyn FnMut(&str, &[usize]),
    ) -> (Tensor<4>, Vec<Tensor<4>>) {
        tap("unet@in", &image.dims());
        tap("unet/encoder@in", &image.dims());
        // Down, keeping what each level saw before it was halved: that is
        // what the matching step up is missing.
        let mut skips = Vec::with_capacity(DEPTH);
        let mut x = image;
        for (level, block) in self.down.iter().enumerate() {
            x = block.forward_tapped(x, &format!("unet/encoder/down{level}"), tap);
            tap(&format!("unet/encoder:skip{level}"), &x.dims());
            skips.push(x.clone());
            x = self.pool.forward(x);
            tap(&format!("unet/encoder/pool{level}"), &x.dims());
        }

        x = self.bottom.forward_tapped(x, "unet/encoder/bottom", tap);
        tap("unet/encoder", &x.dims());
        (x, skips)
    }

    /// The answer in the shape the dataset's labels are in:
    /// `[batch, classes + 2, height, width]` — the probability of each opaque
    /// area, then the sine and the cosine of the angle.
    ///
    /// The class channels are a softmax over the `classes + 1` the head
    /// emits with the frame's dropped, so they sum to one *less* whatever the
    /// network gives the frame: a pixel it is sure is frame comes out all but
    /// zero, which is what a label writes there.
    pub fn predict(&self, image: Tensor<4>) -> Tensor<4> {
        let raw = self.forward(image);
        let [batch, _, height, width] = raw.dims();

        // Softmax down the channel axis, over the class logits alone; the two
        // angle channels are not part of that distribution and are passed
        // through as they are.
        let logits = raw
            .clone()
            .slice([0..batch, 0..self.classes + 1, 0..height, 0..width]);
        let angle = raw.clone().slice([
            0..batch,
            self.classes + 1..self.classes + 3,
            0..height,
            0..width,
        ]);

        let probabilities =
            softmax(logits, 1).slice([0..batch, 0..self.classes, 0..height, 0..width]);
        let mut answer = vec![probabilities, angle];
        if self.lines > 0 {
            // Each line symbol on its own: a sigmoid, not a share of one.
            let lines = raw.clone().slice([
                0..batch,
                self.classes + 3..self.classes + 3 + self.lines,
                0..height,
                0..width,
            ]);
            answer.push(sigmoid(lines));
        }
        if self.points > 0 {
            // A heatmap per point symbol, each on its own.
            let first = self.classes + 3 + self.lines;
            let points =
                raw.clone()
                    .slice([0..batch, first..first + self.points, 0..height, 0..width]);
            answer.push(sigmoid(points));
            if self.point_angles {
                // The angle as it is, as the ground's is.
                let first = first + self.points;
                answer.push(raw.slice([0..batch, first..first + 2, 0..height, 0..width]));
            }
        }
        Tensor::cat(answer, 1)
    }

    /// How many opaque areas this network was built for, which is how many
    /// one-hot channels its answer has.
    pub fn classes(&self) -> usize {
        self.classes
    }

    /// The cross-entropy the class logits are scored with, per-class weights
    /// and all.
    pub fn class_loss(&self) -> &CrossEntropyLoss {
        &self.class_loss
    }

    /// What the angle term counts for beside the class term.
    pub fn angle_weight(&self) -> f64 {
        self.angle_weight
    }

    /// How many channels its answer has in the dataset's shape: one per
    /// opaque area, two for the angle, one per line symbol and one per point
    /// symbol.
    pub fn channels(&self) -> usize {
        self.classes + 2 + self.lines + self.points + if self.point_angles { 2 } else { 0 }
    }

    /// Whether it says which way each point is turned, in two channels after
    /// the heatmaps — see [`UNetConfig::point_angles`].
    pub fn point_angles(&self) -> bool {
        self.point_angles
    }

    /// How many point symbols it answers for, nought for a network with no
    /// points layer.
    pub fn points(&self) -> usize {
        self.points
    }

    /// What the points term counts for beside the ground's.
    pub fn point_weight(&self) -> f64 {
        self.point_weight
    }

    /// How many line symbols it answers for, nought for a network which reads
    /// the ground alone.
    pub fn lines(&self) -> usize {
        self.lines
    }

    /// What the lines term counts for beside the ground's.
    pub fn line_weight(&self) -> f64 {
        self.line_weight
    }

    /// The class the frame is, in the head's own numbering: the one logit
    /// which is not an opaque area, and the target for a pixel no ground
    /// cover reaches.
    pub fn frame_class(&self) -> usize {
        self.classes
    }

    /// How many batch normalization channels have no finite variance to
    /// normalize by, and so emit their own bias wherever the network is not
    /// normalizing by the batch in front of it.
    ///
    /// Nought for a network which was trained and never written down, and
    /// nought for one written down at full precision. What this counts is the
    /// damage half precision does to a record: an `f16` tops out at 65504 and
    /// the variances here reach 1e5, so every channel past that was stored as
    /// an infinity — see [`crate::net::predict`], which is where a loaded
    /// model is checked and where the story is.
    pub fn constant_channels(&self) -> usize {
        self.down
            .iter()
            .chain(std::iter::once(&self.bottom))
            .chain(self.up.iter().map(|block| &block.conv))
            .map(DoubleConv::constant_channels)
            .sum()
    }
}

/// The head which says how dense a picture is: the bottom's features averaged
/// over the whole picture, then a dense layer, a rectifier and one number.
///
/// An average rather than anything which looks at where: the density of a
/// picture is the same all over it, and an average over however much picture
/// there is makes the head answer a tile of any size.
#[derive(Module, Debug)]
pub struct ScaleHead {
    hidden: Linear,
    relu: Relu,
    out: Linear,
}

impl ScaleHead {
    /// `[batch, channels, h, w]` of the bottom to `[batch, 1]`.
    fn forward_tapped(&self, x: Tensor<4>, tap: &mut dyn FnMut(&str, &[usize])) -> Tensor<2> {
        let [batch, channels, ..] = x.dims();
        tap("unet/scale_head@in", &x.dims());
        let pooled = x.mean_dim(3).mean_dim(2);
        tap("unet/scale_head/pool", &pooled.dims());
        let flat: Tensor<2> = pooled.reshape([batch, channels]);
        tap("unet/scale_head/flatten", &flat.dims());
        let hidden = self.hidden.forward(flat);
        tap("unet/scale_head/hidden", &hidden.dims());
        let hidden = self.relu.forward(hidden);
        tap("unet/scale_head/relu", &hidden.dims());
        let out = self.out.forward(hidden);
        tap("unet/scale_head/out", &out.dims());
        tap("unet/scale_head", &out.dims());
        out
    }
}

/// Convolution, normalization, rectifier — twice, which is the block a U-Net
/// is built out of at every level.
///
/// The padding keeps the picture the size it came in at, so the only thing
/// which changes a resolution here is the pooling and the doubling.
#[derive(Module, Debug)]
pub struct DoubleConv {
    first: Conv2d,
    first_norm: BatchNorm,
    second: Conv2d,
    second_norm: BatchNorm,
    relu: Relu,
}

/// What a [`DoubleConv`] is to be built as.
#[derive(Config, Debug)]
pub struct DoubleConvConfig {
    /// Channels in.
    pub in_channels: usize,
    /// Channels out, of both convolutions.
    pub out_channels: usize,
}

impl DoubleConvConfig {
    /// Builds the block on `device`.
    pub fn init(&self, device: &Device) -> DoubleConv {
        let conv = |from: usize, to: usize| {
            Conv2dConfig::new([from, to], [3, 3])
                .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
                // The bias would be subtracted straight back out by the
                // normalization which follows it.
                .with_bias(false)
                .init(device)
        };
        DoubleConv {
            first: conv(self.in_channels, self.out_channels),
            first_norm: BatchNormConfig::new(self.out_channels).init(device),
            second: conv(self.out_channels, self.out_channels),
            second_norm: BatchNormConfig::new(self.out_channels).init(device),
            relu: Relu::new(),
        }
    }
}

impl DoubleConv {
    /// How many of this block's two normalizations' channels have no finite
    /// variance — see [`UNet::constant_channels`].
    fn constant_channels(&self) -> usize {
        [&self.first_norm, &self.second_norm]
            .into_iter()
            .map(|norm| {
                norm.running_var
                    .value()
                    .into_data()
                    .iter::<f32>()
                    .filter(|variance| !variance.is_finite())
                    .count()
            })
            .sum()
    }

    /// `[batch, in, h, w]` to `[batch, out, h, w]`.
    pub fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        self.forward_tapped(x, "", &mut |_, _| {})
    }

    /// [`DoubleConv::forward`], reporting each tensor to `tap` as
    /// `{path}/conv1`, `{path}/norm1`, … — see [`describe_double_conv`].
    pub(crate) fn forward_tapped(
        &self,
        x: Tensor<4>,
        path: &str,
        tap: &mut dyn FnMut(&str, &[usize]),
    ) -> Tensor<4> {
        tap(&format!("{path}@in"), &x.dims());
        let x = self.first.forward(x);
        tap(&format!("{path}/conv1"), &x.dims());
        let x = self.first_norm.forward(x);
        tap(&format!("{path}/norm1"), &x.dims());
        let x = self.relu.forward(x);
        tap(&format!("{path}/relu1"), &x.dims());
        let x = self.second.forward(x);
        tap(&format!("{path}/conv2"), &x.dims());
        let x = self.second_norm.forward(x);
        tap(&format!("{path}/norm2"), &x.dims());
        let x = self.relu.forward(x);
        tap(&format!("{path}/relu2"), &x.dims());
        tap(path, &x.dims());
        x
    }
}

/// One step back up: double the picture, put the skip connection back beside
/// it, and make something of the two together.
#[derive(Module, Debug)]
pub struct UpBlock {
    /// The learned doubling. A plain interpolation would do, but a
    /// transposed convolution lets the network decide what a doubled pixel
    /// looks like, which is most of what the decoder is for.
    upsample: ConvTranspose2d,
    /// Reads the doubled picture and the skip connection as one.
    conv: DoubleConv,
}

/// What an [`UpBlock`] is to be built as.
#[derive(Config, Debug)]
pub struct UpBlockConfig {
    /// Channels coming up from below.
    pub in_channels: usize,
    /// Channels out, which is also what the skip connection brings.
    pub out_channels: usize,
}

impl UpBlockConfig {
    /// Builds the block on `device`.
    pub fn init(&self, device: &Device) -> UpBlock {
        UpBlock {
            // Kernel two, stride two: every pixel becomes exactly the two by
            // two it was pooled out of, with no overlap to leave a
            // chequerboard behind.
            upsample: ConvTranspose2dConfig::new([self.in_channels, self.out_channels], [2, 2])
                .with_stride([2, 2])
                .init(device),
            // Twice the channels in: the doubled picture and the skip
            // connection, side by side.
            conv: DoubleConvConfig::new(self.out_channels * 2, self.out_channels).init(device),
        }
    }
}

impl UpBlock {
    /// Doubles `x`, concatenates `skip` onto it and convolves the two.
    pub fn forward(&self, x: Tensor<4>, skip: Tensor<4>) -> Tensor<4> {
        self.forward_tapped(x, skip, "", &mut |_, _| {})
    }

    /// [`UpBlock::forward`], reporting each tensor to `tap` under `path`.
    pub(crate) fn forward_tapped(
        &self,
        x: Tensor<4>,
        skip: Tensor<4>,
        path: &str,
        tap: &mut dyn FnMut(&str, &[usize]),
    ) -> Tensor<4> {
        tap(&format!("{path}@in:x"), &x.dims());
        tap(&format!("{path}@in:skip"), &skip.dims());
        let x = self.upsample.forward(x);
        tap(&format!("{path}/upsample"), &x.dims());
        let x = Tensor::cat(vec![x, skip], 1);
        tap(&format!("{path}/concat"), &x.dims());
        let x = self.conv.forward_tapped(x, &format!("{path}/conv"), tap);
        tap(path, &x.dims());
        x
    }
}

/// The descriptor of a [`DoubleConv`] from `from` to `to` channels, on a
/// picture of `h` by `w` (symbolic or not): what [`DoubleConv::forward_tapped`]
/// reports, node for node.
pub(crate) fn describe_double_conv(id: &str, from: usize, to: usize, h: &str, w: &str) -> Node {
    let shape = |channels: usize| -> [String; 4] {
        [
            "B".to_string(),
            channels.to_string(),
            h.to_string(),
            w.to_string(),
        ]
    };
    let [b, c_in, hh, ww] = shape(from);
    let [_, c_out, _, _] = shape(to);
    let edge = |node: Node, a: &str, z: &str, dims: &[&str; 4]| node.edge(a, z, dims);
    let node = Node::new(id, "double_conv", "Double conv")
        .summary(&format!("2 × (conv 3×3, batch norm, ReLU), {from}→{to}"))
        .param("in_channels", from)
        .param("out_channels", to)
        .child(layer::conv2d("conv1", from, to, 3, false))
        .child(layer::batch_norm("norm1", to))
        .child(layer::relu("relu1"))
        .child(layer::conv2d("conv2", to, to, 3, false))
        .child(layer::batch_norm("norm2", to))
        .child(layer::relu("relu2"));
    let i = [b.as_str(), c_in.as_str(), hh.as_str(), ww.as_str()];
    let o = [b.as_str(), c_out.as_str(), hh.as_str(), ww.as_str()];
    let node = edge(node, "@in", "conv1", &i);
    let node = edge(node, "conv1", "norm1", &o);
    let node = edge(node, "norm1", "relu1", &o);
    let node = edge(node, "relu1", "conv2", &o);
    let node = edge(node, "conv2", "norm2", &o);
    let node = edge(node, "norm2", "relu2", &o);
    edge(node, "relu2", "@out", &o)
}

/// The descriptor of an [`UpBlock`] taking `from` channels at half the
/// picture of `h` by `w` and the skip connection's `to`, to `to` at full size.
fn describe_up_block(id: &str, from: usize, to: usize, level: usize) -> Node {
    let (h, w) = (halved("H", level), halved("W", level));
    let (h_below, w_below) = (halved("H", level + 1), halved("W", level + 1));
    let (from_s, to_s, cat_s) = (from.to_string(), to.to_string(), (2 * to).to_string());
    Node::new(id, "up_block", "Up block")
        .summary("double the picture, join the skip connection, convolve")
        .param("in_channels", from)
        .param("out_channels", to)
        .child(layer::upsample("upsample", from, to))
        .child(
            Node::new("concat", "concat", "Concat")
                .summary("skip connection beside the doubled picture")
                .param("axis", 1),
        )
        .child(describe_double_conv("conv", 2 * to, to, &h, &w))
        .edge("@in:x", "upsample", &["B", &from_s, &h_below, &w_below])
        .edge("upsample", "concat:x", &["B", &to_s, &h, &w])
        .edge("@in:skip", "concat:skip", &["B", &to_s, &h, &w])
        .edge("concat", "conv", &["B", &cat_s, &h, &w])
        .edge("conv", "@out", &["B", &to_s, &h, &w])
}

impl UNetConfig {
    /// What this network is, as [`crate::net::describe`] writes it down. Keep
    /// it beside `UNet::forward_tapped`: a test runs one against the other.
    ///
    /// Three blocks, the encoder, the decoder and the head, so that the whole
    /// reads at a glance; the skip connections between the first two are the
    /// encoder's `@out:skip<level>` ports and the decoder's `@in:skip<level>`
    /// ones, one for each level.
    pub fn describe(&self) -> NetDescriptor {
        let width = |level: usize| self.base_channels << level;
        let shape = |channels: usize, level: usize| -> [String; 4] {
            [
                "B".to_string(),
                channels.to_string(),
                halved("H", level),
                halved("W", level),
            ]
        };
        fn refs(dims: &[String; 4]) -> [&str; 4] {
            [
                dims[0].as_str(),
                dims[1].as_str(),
                dims[2].as_str(),
                dims[3].as_str(),
            ]
        }

        // The way down: a double convolution and a halving at each level, and
        // a double convolution at the bottom. What each level made is also
        // what the matching level of the decoder is handed.
        let mut encoder = Node::new("encoder", "group", "Encoder")
            .summary(&format!(
                "{DEPTH} × (double conv, max pool), then the bottom: what is in the picture"
            ))
            .param("levels", DEPTH);
        let mut from = self.in_channels;
        for level in 0..DEPTH {
            let to = width(level);
            let (h, w) = (halved("H", level), halved("W", level));
            let before = if level == 0 {
                "@in".to_string()
            } else {
                format!("pool{}", level - 1)
            };
            encoder = encoder
                .child(describe_double_conv(
                    &format!("down{level}"),
                    from,
                    to,
                    &h,
                    &w,
                ))
                .child(layer::max_pool(&format!("pool{level}")))
                .edge(&before, &format!("down{level}"), &refs(&shape(from, level)))
                .edge(
                    &format!("down{level}"),
                    &format!("pool{level}"),
                    &refs(&shape(to, level)),
                )
                .edge(
                    &format!("down{level}"),
                    &format!("@out:skip{level}"),
                    &refs(&shape(to, level)),
                );
            from = to;
        }
        encoder = encoder
            .child(describe_double_conv(
                "bottom",
                from,
                width(DEPTH),
                &halved("H", DEPTH),
                &halved("W", DEPTH),
            ))
            .edge(
                &format!("pool{}", DEPTH - 1),
                "bottom",
                &refs(&shape(from, DEPTH)),
            )
            .edge("bottom", "@out", &refs(&shape(width(DEPTH), DEPTH)));

        // The way back up, deepest first, each step handed the level it lost.
        let mut decoder = Node::new("decoder", "group", "Decoder")
            .summary(&format!(
                "{DEPTH} × (double the picture, join a skip connection, convolve): where it is"
            ))
            .param("levels", DEPTH);
        for step in 0..DEPTH {
            let level = DEPTH - 1 - step;
            decoder = decoder
                .child(describe_up_block(
                    &format!("up{step}"),
                    width(level + 1),
                    width(level),
                    level,
                ))
                .edge(
                    &format!("@in:skip{level}"),
                    &format!("up{step}:skip"),
                    &refs(&shape(width(level), level)),
                );
        }
        decoder = decoder.edge("@in:x", "up0:x", &refs(&shape(width(DEPTH), DEPTH)));
        for step in 0..DEPTH - 1 {
            let level = DEPTH - 1 - step;
            decoder = decoder.edge(
                &format!("up{step}"),
                &format!("up{}:x", step + 1),
                &refs(&shape(width(level), level)),
            );
        }
        decoder = decoder.edge(
            &format!("up{}", DEPTH - 1),
            "@out",
            &refs(&shape(width(0), 0)),
        );

        let head = layer::conv2d("head", self.base_channels, self.head_channels(), 1, true)
            .summary(&format!(
                "{} class logits (the frame included), sine and cosine of the angle{}{}",
                self.classes + 1,
                if self.lines > 0 {
                    format!(", then {} line logits", self.lines)
                } else {
                    String::new()
                },
                match (self.points, self.point_angle_channels()) {
                    (0, _) => String::new(),
                    (points, 0) => format!(", then {points} point heatmap logits"),
                    (points, _) => format!(
                        ", then {points} point heatmap logits and the sine and cosine of the \
                         points' angle"
                    ),
                }
            ));

        let mut root = Node::new("unet", "model", "U-Net")
            .summary("reads a rendered map, pixel by pixel, into symbols and angles")
            .param("depth", DEPTH)
            .child(encoder)
            .child(decoder)
            .child(head)
            .edge("@in", "encoder", &refs(&shape(self.in_channels, 0)))
            .edge("encoder", "decoder:x", &refs(&shape(width(DEPTH), DEPTH)));
        for level in 0..DEPTH {
            root = root.edge(
                &format!("encoder:skip{level}"),
                &format!("decoder:skip{level}"),
                &refs(&shape(width(level), level)),
            );
        }
        root = root
            .edge("decoder", "head", &refs(&shape(width(0), 0)))
            .edge("head", "@out", &refs(&shape(self.head_channels(), 0)));

        // Only where there is one, so that a network without it describes
        // exactly as it did before there was such a thing.
        if self.scale_head {
            let bottom = width(DEPTH);
            let b = bottom.to_string();
            let hidden = SCALE_HIDDEN.to_string();
            let scale_head = Node::new("scale_head", "group", "Scale head")
                .summary("how much denser than the normal the picture is, as a natural logarithm")
                .child(
                    Node::new("pool", "global_avg_pool", "Global average pool")
                        .summary("the bottom's features averaged over the whole picture"),
                )
                .child(Node::new("flatten", "flatten", "Flatten"))
                .child(layer::linear("hidden", bottom, SCALE_HIDDEN))
                .child(layer::relu("relu"))
                .child(layer::linear("out", SCALE_HIDDEN, 1))
                .edge("@in", "pool", &refs(&shape(bottom, DEPTH)))
                .edge("pool", "flatten", &["B", &b, "1", "1"])
                .edge("flatten", "hidden", &["B", &b])
                .edge("hidden", "relu", &["B", &hidden])
                .edge("relu", "out", &["B", &hidden])
                .edge("out", "@out", &["B", "1"]);
            root = root
                .child(scale_head)
                .edge("encoder", "scale_head", &refs(&shape(bottom, DEPTH)))
                .edge("scale_head", "@out:scale", &["B", "1"]);
        }

        let mut config = BTreeMap::new();
        config.insert("classes".to_string(), Value::from(self.classes));
        config.insert("in_channels".to_string(), Value::from(self.in_channels));
        config.insert("base_channels".to_string(), Value::from(self.base_channels));
        if self.scale_head {
            config.insert("scale_head".to_string(), Value::from(true));
        }
        NetDescriptor::new("image_to_omap", "unet", config, root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_head_is_a_class_per_symbol_the_frame_and_the_angle() {
        let device = Device::flex();
        let net = UNetConfig::new(5).with_base_channels(4).init(&device);

        // Sixteen divides by two four times, which is what the depth asks.
        let image = Tensor::<4>::zeros([2, 3, 16, 16], &device);
        // Five symbols, the frame, and the sine and the cosine.
        assert_eq!(net.forward(image.clone()).dims(), [2, 5 + 3, 16, 16]);
        // And the answer is the dataset's own shape: no frame channel.
        assert_eq!(net.predict(image).dims(), [2, 5 + 2, 16, 16]);
        assert_eq!(net.channels(), 7);
        assert_eq!(net.frame_class(), 5);
    }

    /// The class channels of an answer are a distribution over the symbols
    /// and the frame, so what is left of them once the frame is dropped is a
    /// share of one rather than one.
    #[test]
    fn the_class_channels_of_an_answer_are_probabilities() {
        let device = Device::flex();
        let net = UNetConfig::new(3).with_base_channels(4).init(&device);

        let answer = net.predict(Tensor::zeros([1, 3, 16, 16], &device));
        let [_, channels, height, width] = answer.dims();
        assert_eq!(channels, 5);

        let classes = answer.slice([0..1, 0..3, 0..height, 0..width]);
        assert!(classes
            .clone()
            .greater_equal_elem(0.0)
            .all()
            .into_scalar::<bool>());
        let total: f32 = classes.sum().into_scalar();
        // Three of the four the softmax spread itself over: under one pixel
        // for pixel, and nowhere near nothing.
        let pixels = (height * width) as f32;
        assert!(
            total < pixels && total > 0.0,
            "{total} over {pixels} pixels"
        );
    }

    /// The descriptor says what the forward pass does: every edge's shape is
    /// the shape of the tensor the pass made there, and the weights counted
    /// add up to the module's own.
    #[test]
    fn the_descriptor_matches_the_forward_pass() {
        let device = Device::flex();
        let config = UNetConfig::new(5).with_base_channels(4);
        let net = config.init(&device);
        let descriptor = config.describe();

        let mut reported = Vec::new();
        let image = Tensor::<4>::zeros([2, 3, 32, 48], &device);
        net.forward_tapped(image, &mut |key, dims| {
            reported.push((key.to_string(), dims.to_vec()));
        });
        crate::net::describe::assert_matches_forward(
            &descriptor,
            &reported,
            &[("B", 2), ("H", 32), ("W", 48)],
        );
        assert_eq!(descriptor.total_params(), net.num_params() as u64);
        assert_eq!(descriptor.problems(), Vec::<String>::new());
    }

    /// A network with a lines layer has a channel per line symbol after the
    /// ground's, as a probability, and one without is what it always was.
    #[test]
    fn the_lines_layer_is_a_sigmoid_channel_per_line_symbol() {
        let device = Device::flex();
        let net = UNetConfig::new(5)
            .with_base_channels(4)
            .with_lines(3)
            .init(&device);
        let answer = net.predict(Tensor::zeros([1, 3, 16, 16], &device));
        assert_eq!(answer.dims(), [1, 5 + 2 + 3, 16, 16]);
        assert_eq!(net.channels(), 10);

        let lines = answer.slice([0..1, 7..10, 0..16, 0..16]);
        let values = lines.into_data().try_to_vec::<f32>().expect("floats");
        assert!(values.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    /// A network with a points layer has a heatmap per point symbol after
    /// the lines', as a probability which starts out low everywhere.
    #[test]
    fn the_points_layer_is_a_sigmoid_channel_per_point_symbol_starting_low() {
        let device = Device::flex();
        let config = UNetConfig::new(5)
            .with_base_channels(4)
            .with_lines(3)
            .with_points(2);
        let net = config.init(&device);
        let answer = net.predict(Tensor::zeros([1, 3, 16, 16], &device));
        assert_eq!(answer.dims(), [1, 5 + 2 + 3 + 2, 16, 16]);
        assert_eq!(net.channels(), 12);
        assert_eq!(net.points(), 2);

        let points = answer.slice([0..1, 10..12, 0..16, 0..16]);
        let values = points.into_data().try_to_vec::<f32>().expect("floats");
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        assert!(mean < 0.1, "a point is unlikely to start with: {mean}");

        // And the descriptor holds to the forward pass.
        let descriptor = config.describe();
        let mut reported = Vec::new();
        net.forward_tapped(
            Tensor::<4>::zeros([2, 3, 32, 48], &device),
            &mut |key, dims| {
                reported.push((key.to_string(), dims.to_vec()));
            },
        );
        crate::net::describe::assert_matches_forward(
            &descriptor,
            &reported,
            &[("B", 2), ("H", 32), ("W", 48)],
        );
        assert_eq!(descriptor.total_params(), net.num_params() as u64);
    }

    /// A network which says which way its points are turned has two more
    /// channels after the heatmaps, which start at nought rather than at the
    /// heatmaps' low prior, and its descriptor holds to its forward pass.
    #[test]
    fn the_points_angle_is_two_channels_after_the_heatmaps() {
        let device = Device::flex();
        let config = UNetConfig::new(5)
            .with_base_channels(4)
            .with_lines(3)
            .with_points(2)
            .with_point_angles(true);
        let net = config.init(&device);
        let answer = net.predict(Tensor::zeros([1, 3, 16, 16], &device));
        assert_eq!(answer.dims(), [1, 5 + 2 + 3 + 2 + 2, 16, 16]);
        assert_eq!(net.channels(), 14);
        assert!(net.point_angles());
        let angle = answer.slice([0..1, 12..14, 0..16, 0..16]);
        let values = angle.into_data().try_to_vec::<f32>().expect("floats");
        let mean = values.iter().map(|v| v.abs()).sum::<f32>() / values.len() as f32;
        assert!(mean < 1.0, "the angle starts out near no angle: {mean}");

        let descriptor = config.describe();
        let mut reported = Vec::new();
        net.forward_tapped(
            Tensor::<4>::zeros([2, 3, 32, 48], &device),
            &mut |key, dims| {
                reported.push((key.to_string(), dims.to_vec()));
            },
        );
        crate::net::describe::assert_matches_forward(
            &descriptor,
            &reported,
            &[("B", 2), ("H", 32), ("W", 48)],
        );
        assert_eq!(descriptor.total_params(), net.num_params() as u64);
        // With no points, there is no angle of them to give.
        assert_eq!(
            UNetConfig::new(5).with_point_angles(true).head_channels(),
            UNetConfig::new(5).head_channels()
        );
    }

    /// The descriptor of a network with lines holds to its forward pass too.
    #[test]
    fn the_descriptor_matches_the_forward_pass_with_lines() {
        let device = Device::flex();
        let config = UNetConfig::new(5).with_base_channels(4).with_lines(3);
        let net = config.init(&device);
        let descriptor = config.describe();

        let mut reported = Vec::new();
        net.forward_tapped(
            Tensor::<4>::zeros([2, 3, 32, 48], &device),
            &mut |key, dims| {
                reported.push((key.to_string(), dims.to_vec()));
            },
        );
        crate::net::describe::assert_matches_forward(
            &descriptor,
            &reported,
            &[("B", 2), ("H", 32), ("W", 48)],
        );
        assert_eq!(descriptor.total_params(), net.num_params() as u64);
    }

    /// The scale head answers one number per picture, whatever size the
    /// picture is, and the same one out of `scale` as out of `forward_all`;
    /// a network without one has nothing to say.
    #[test]
    fn the_scale_head_answers_one_number_per_picture() {
        let device = Device::flex();
        let net = UNetConfig::new(5)
            .with_base_channels(4)
            .with_scale_head(true)
            .init(&device);
        assert!(net.has_scale_head());
        for (height, width) in [(16, 16), (32, 48)] {
            let image = Tensor::<4>::ones([3, 3, height, width], &device);
            let (raw, scale) = net.forward_all(image.clone());
            assert_eq!(raw.dims(), [3, 5 + 3, height, width]);
            let scale = scale.expect("a network with a scale head");
            assert_eq!(scale.dims(), [3, 1]);
            let alone = net.scale(image).expect("the same head");
            let (a, b): (Vec<f32>, Vec<f32>) = (
                scale.into_data().try_to_vec().expect("floats"),
                alone.into_data().try_to_vec().expect("floats"),
            );
            for (a, b) in a.iter().zip(&b) {
                assert!((a - b).abs() < 1e-5, "{a} and {b}");
            }
        }

        let without = UNetConfig::new(5).with_base_channels(4).init(&device);
        assert!(!without.has_scale_head());
        assert!(without
            .scale(Tensor::zeros([1, 3, 16, 16], &device))
            .is_none());
    }

    /// The descriptor of a network with a scale head holds to its forward
    /// pass too, and one without describes exactly as it did before there
    /// was such a head.
    #[test]
    fn the_descriptor_matches_the_forward_pass_with_a_scale_head() {
        let device = Device::flex();
        let config = UNetConfig::new(5)
            .with_base_channels(4)
            .with_lines(3)
            .with_scale_head(true);
        let net = config.init(&device);
        let descriptor = config.describe();

        let mut reported = Vec::new();
        net.forward_tapped(
            Tensor::<4>::zeros([2, 3, 32, 48], &device),
            &mut |key, dims| {
                reported.push((key.to_string(), dims.to_vec()));
            },
        );
        crate::net::describe::assert_matches_forward(
            &descriptor,
            &reported,
            &[("B", 2), ("H", 32), ("W", 48)],
        );
        assert_eq!(descriptor.total_params(), net.num_params() as u64);
        assert_eq!(descriptor.problems(), Vec::<String>::new());

        let plain = UNetConfig::new(5).with_base_channels(4).with_lines(3);
        assert_ne!(
            plain.describe().canonical_hash(),
            descriptor.canonical_hash()
        );
        assert_eq!(
            plain.describe().canonical_hash(),
            plain
                .clone()
                .with_scale_weight(3.0)
                .describe()
                .canonical_hash(),
            "the weight of a term is training, not architecture"
        );
    }

    #[test]
    fn the_training_memory_grows_with_the_batch_and_the_crop() {
        let config = UNetConfig::new(10);
        let one = config.training_bytes(1, 256).unwrap();
        let eight = config.training_bytes(8, 256).unwrap();
        // The weights are paid once, the activations per crop.
        assert!(eight > 7 * one && eight < 8 * one, "{one} {eight}");
        assert!(config.training_bytes(1, 512).unwrap() > 3 * one);
        assert!(
            config
                .with_base_channels(32)
                .training_bytes(1, 256)
                .unwrap()
                > one
        );
        assert_eq!(UNetConfig::new(10).training_bytes(1, 250), None);
        // Base 16, crops of 256: between a hundred megabytes and a gigabyte a crop.
        assert!((100 << 20..1 << 30).contains(&one), "{one}");
    }
}
