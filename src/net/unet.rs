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
//! [`UNet::forward`] is those `on + 3` raw channels, which is what the loss
//! wants. [`UNet::predict`] is the answer in the dataset's own shape: the
//! softmax over the `on + 1`, with the frame channel dropped, and the two
//! angle channels after it — `on + 2`, matching a `gt/*.bin` expanded by
//! [`crate::ground_truth::GroundTruth::one_hot`] channel for channel.
//!
//! Everything here is `NCHW`, which is the layout burn's convolutions work
//! in: `[batch, channel, row, column]`. The label on disk is `H, W, C`, the
//! same numbers read in a different order, and [`crate::net::data`] is where
//! the order changes — once, while a batch is built, rather than on every
//! tensor that passes through here.

use burn::config::Config;
use burn::module::Module;
use burn::nn::conv::{Conv2d, Conv2dConfig, ConvTranspose2d, ConvTranspose2dConfig};
use burn::nn::loss::{CrossEntropyLoss, CrossEntropyLossConfig};
use burn::nn::pool::{MaxPool2d, MaxPool2dConfig};
use burn::nn::{BatchNorm, BatchNormConfig, PaddingConfig2d, Relu};
use burn::prelude::Backend;
use burn::tensor::activation::softmax;
use burn::tensor::Tensor;

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
}

impl UNetConfig {
    /// Builds the network on `device`, with its weights initialized.
    pub fn init<B: Backend>(&self, device: &B::Device) -> UNet<B> {
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
            head: Conv2dConfig::new([self.base_channels, self.classes + 3], [1, 1]).init(device),
            classes: self.classes,
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

/// A U-Net which reads a rendered map back into the symbols it was drawn
/// with.
#[derive(Module, Debug)]
pub struct UNet<B: Backend> {
    /// The encoder, one block per level, full resolution first.
    down: Vec<DoubleConv<B>>,
    /// What halves the picture between one level and the next.
    pool: MaxPool2d,
    /// The block at the bottom, between the last halving and the first
    /// doubling.
    bottom: DoubleConv<B>,
    /// The decoder, deepest first.
    up: Vec<UpBlock<B>>,
    /// The 1x1 convolution which turns features into an answer.
    head: Conv2d<B>,
    /// How many opaque areas the symbol set holds — see [`UNetConfig`].
    classes: usize,
    /// The cross-entropy the class logits are scored with, its per-class
    /// weights already in it — see [`UNetConfig::class_weights`].
    class_loss: CrossEntropyLoss<B>,
    /// What the angle term counts for — see [`UNetConfig::angle_weight`].
    angle_weight: f64,
}

impl<B: Backend> UNet<B> {
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
    pub fn forward(&self, image: Tensor<B, 4>) -> Tensor<B, 4> {
        // Down, keeping what each level saw before it was halved: that is
        // what the matching step up is missing.
        let mut skips = Vec::with_capacity(DEPTH);
        let mut x = image;
        for block in &self.down {
            x = block.forward(x);
            skips.push(x.clone());
            x = self.pool.forward(x);
        }

        x = self.bottom.forward(x);

        // And up, deepest first, each step handed back the level it lost.
        for (block, skip) in self.up.iter().zip(skips.into_iter().rev()) {
            x = block.forward(x, skip);
        }

        self.head.forward(x)
    }

    /// The answer in the shape the dataset's labels are in:
    /// `[batch, classes + 2, height, width]` — the probability of each opaque
    /// area, then the sine and the cosine of the angle.
    ///
    /// The class channels are a softmax over the `classes + 1` the head
    /// emits with the frame's dropped, so they sum to one *less* whatever the
    /// network gives the frame: a pixel it is sure is frame comes out all but
    /// zero, which is what a label writes there.
    pub fn predict(&self, image: Tensor<B, 4>) -> Tensor<B, 4> {
        let raw = self.forward(image);
        let [batch, _, height, width] = raw.dims();

        // Softmax down the channel axis, over the class logits alone; the two
        // angle channels are not part of that distribution and are passed
        // through as they are.
        let logits = raw
            .clone()
            .slice([0..batch, 0..self.classes + 1, 0..height, 0..width]);
        let angle = raw.slice([
            0..batch,
            self.classes + 1..self.classes + 3,
            0..height,
            0..width,
        ]);

        let probabilities =
            softmax(logits, 1).slice([0..batch, 0..self.classes, 0..height, 0..width]);
        Tensor::cat(vec![probabilities, angle], 1)
    }

    /// How many opaque areas this network was built for, which is how many
    /// one-hot channels its answer has.
    pub fn classes(&self) -> usize {
        self.classes
    }

    /// The cross-entropy the class logits are scored with, per-class weights
    /// and all.
    pub fn class_loss(&self) -> &CrossEntropyLoss<B> {
        &self.class_loss
    }

    /// What the angle term counts for beside the class term.
    pub fn angle_weight(&self) -> f64 {
        self.angle_weight
    }

    /// How many channels its answer has in the dataset's shape: one per
    /// opaque area, and two for the angle.
    pub fn channels(&self) -> usize {
        self.classes + 2
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

/// Convolution, normalization, rectifier — twice, which is the block a U-Net
/// is built out of at every level.
///
/// The padding keeps the picture the size it came in at, so the only thing
/// which changes a resolution here is the pooling and the doubling.
#[derive(Module, Debug)]
pub struct DoubleConv<B: Backend> {
    first: Conv2d<B>,
    first_norm: BatchNorm<B, 2>,
    second: Conv2d<B>,
    second_norm: BatchNorm<B, 2>,
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
    pub fn init<B: Backend>(&self, device: &B::Device) -> DoubleConv<B> {
        let conv = |from: usize, to: usize| {
            Conv2dConfig::new([from, to], [3, 3])
                .with_padding(PaddingConfig2d::Explicit(1, 1))
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

impl<B: Backend> DoubleConv<B> {
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
    pub fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let x = self
            .relu
            .forward(self.first_norm.forward(self.first.forward(x)));
        self.relu
            .forward(self.second_norm.forward(self.second.forward(x)))
    }
}

/// One step back up: double the picture, put the skip connection back beside
/// it, and make something of the two together.
#[derive(Module, Debug)]
pub struct UpBlock<B: Backend> {
    /// The learned doubling. A plain interpolation would do, but a
    /// transposed convolution lets the network decide what a doubled pixel
    /// looks like, which is most of what the decoder is for.
    upsample: ConvTranspose2d<B>,
    /// Reads the doubled picture and the skip connection as one.
    conv: DoubleConv<B>,
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
    pub fn init<B: Backend>(&self, device: &B::Device) -> UpBlock<B> {
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

impl<B: Backend> UpBlock<B> {
    /// Doubles `x`, concatenates `skip` onto it and convolves the two.
    pub fn forward(&self, x: Tensor<B, 4>, skip: Tensor<B, 4>) -> Tensor<B, 4> {
        let x = self.upsample.forward(x);
        self.conv.forward(Tensor::cat(vec![x, skip], 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn::backend::ndarray::{NdArray, NdArrayDevice};

    type TestBackend = NdArray<f32>;

    #[test]
    fn the_head_is_a_class_per_symbol_the_frame_and_the_angle() {
        let device = NdArrayDevice::default();
        let net = UNetConfig::new(5)
            .with_base_channels(4)
            .init::<TestBackend>(&device);

        // Sixteen divides by two four times, which is what the depth asks.
        let image = Tensor::<TestBackend, 4>::zeros([2, 3, 16, 16], &device);
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
        let device = NdArrayDevice::default();
        let net = UNetConfig::new(3)
            .with_base_channels(4)
            .init::<TestBackend>(&device);

        let answer = net.predict(Tensor::zeros([1, 3, 16, 16], &device));
        let [_, channels, height, width] = answer.dims();
        assert_eq!(channels, 5);

        let classes = answer.slice([0..1, 0..3, 0..height, 0..width]);
        assert!(classes.clone().greater_equal_elem(0.0).all().into_scalar());
        let total: f32 = classes.sum().into_scalar();
        // Three of the four the softmax spread itself over: under one pixel
        // for pixel, and nowhere near nothing.
        let pixels = (height * width) as f32;
        assert!(
            total < pixels && total > 0.0,
            "{total} over {pixels} pixels"
        );
    }
}
