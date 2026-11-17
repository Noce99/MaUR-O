//! A network which reads a map as a tree of cuts instead of a class per pixel.
//!
//! [`crate::net::unet`] answers "which symbol is this pixel", one pixel at a
//! time, and a picture of areas has to be vectorized afterwards. This answers
//! with the vectors: the picture is **split in two by a curve, and each half
//! is split again**, `depth` times over. What is left at the bottom are
//! `2^depth` leaves, and each carries one symbol.
//!
//! ```text
//! the square ──curve 0──► A ──curve 1──► A1  (a symbol)
//!                     │        └─curve 2──► A2  (a symbol)
//!                     └──► B ──curve 3──► B1 ...
//! ```
//!
//! That is exactly "every curve runs from a border or an earlier curve to a
//! border or an earlier curve": a curve only matters inside the piece it
//! splits, so it stops where that piece does. The leaves tile the square —
//! no overlap, no gap, borders shared — by construction, and a leaf's symbol
//! belongs to the leaf, so there is no question of which area gets which
//! symbol. Neighbouring leaves with the same symbol are one area.
//!
//! # Soft, so it can be learned
//!
//! Which side of a curve a pixel is on is a step, and a step has no
//! gradient. So a pixel belongs to a piece to a *degree*: the piece's share
//! `m` is split between the two halves by `sigmoid(distance / temperature)`,
//! where the distance is to the curve and the sign says which side of it the
//! pixel is on (see `signed_distance` for why that is the polygon's inside
//! and not the nearest segment's side):
//!
//! ```text
//! m_first  = m * sigmoid(s / t)
//! m_second = m * (1 - sigmoid(s / t))
//! ```
//!
//! The leaves' shares add up to one at every pixel, whatever the curves do.
//! The picture is read as
//! `P(class) = sum over leaves of share * softmax(leaf logits)[class]` and
//! scored against the label with a negative log-likelihood, which sends a
//! gradient to the curves, the symbols and the encoder at once. The
//! temperature is what the training anneals: warm, the cuts are blurred and
//! feel each other; cold, they are the cuts the vectors will be.
//!
//! No ground truth beyond the label grid [`crate::net::data`] already
//! provides is needed.
//!
//! # The numbers
//!
//! Coordinates are the picture's, `[0, 1]` across and down. A curve is a
//! Bézier of `control_points` points, `cp = 0.5 + 0.75 * tanh(raw)`, so it
//! can leave the square a little. It is continued past both ends in a
//! straight line, which is what lets a cut reach the border of whatever it
//! cuts however short it is drawn.

use burn::config::Config;
use burn::module::{Module, Param};
use burn::nn::pool::{MaxPool2d, MaxPool2dConfig};
use burn::nn::{Linear, LinearConfig, Relu};
use burn::prelude::Backend;
use burn::tensor::activation::{sigmoid, softmax};
use burn::tensor::{Int, Tensor, TensorData};

use crate::net::unet::{DoubleConv, DoubleConvConfig};

/// Halvings of the encoder, as in the U-Net: the picture's side must divide
/// by `2^LEVELS`.
const LEVELS: usize = 4;

/// How far past its two ends a curve is continued, and how far off the far
/// side of its half-plane polygon is: the numbers [`crate::tree_vector`] draws
/// its polygons with, because what the network trains on has to be what is
/// drawn.
const EXTENSION: f32 = crate::tree_vector::EXTENSION as f32;
const FAR: f32 = crate::tree_vector::FAR as f32;

/// What a [`TreeNet`] is built from.
#[derive(Config, Debug)]
pub struct TreeNetConfig {
    /// How many opaque areas the symbol set holds. The net answers over
    /// `classes + 1`: the frame is a class here too.
    pub classes: usize,
    /// The side of the pictures it is given, in pixels. Must divide by 16.
    #[config(default = 64)]
    pub size: usize,
    /// Cuts from the root to a leaf. `2^depth` leaves, `2^depth - 1` curves.
    #[config(default = 4)]
    pub depth: usize,
    /// Control points of each curve's Bézier.
    #[config(default = 6)]
    pub control_points: usize,
    /// Points a curve is sampled at, to measure the distance to it.
    #[config(default = 24)]
    pub samples: usize,
    /// Channels of the encoder's first level, doubled at each one.
    #[config(default = 8)]
    pub base_channels: usize,
    /// Width of the layer between the encoder and the two heads.
    #[config(default = 256)]
    pub hidden: usize,
}

/// A network which answers a picture with a tree of curves and the symbol of
/// each leaf.
#[derive(Module, Debug)]
pub struct TreeNet<B: Backend> {
    down: Vec<DoubleConv<B>>,
    pool: MaxPool2d,
    hidden: Linear<B>,
    curves: Linear<B>,
    leaves: Linear<B>,
    relu: Relu,
    classes: usize,
    depth: usize,
    control_points: usize,
    samples: usize,
    size: usize,
}

impl TreeNetConfig {
    /// Builds the network on `device`, its weights initialized.
    ///
    /// The curves' head starts small, around a bias which draws each cut as a
    /// straight line through the middle at its own angle (the golden angle
    /// apart): a start where every curve is distinct, since a start where
    /// they are all the same is one the tree never breaks out of.
    pub fn init<B: Backend>(&self, device: &B::Device) -> TreeNet<B> {
        assert!(
            self.size.is_multiple_of(1 << LEVELS),
            "the side must divide by {}",
            1 << LEVELS
        );
        let mut down = Vec::with_capacity(LEVELS);
        let mut from = 3;
        for level in 0..LEVELS {
            let to = self.base_channels << level;
            down.push(DoubleConvConfig::new(from, to).init(device));
            from = to;
        }
        let side = self.size >> LEVELS;
        let flat = from * side * side;

        let nodes = (1 << self.depth) - 1;
        let leaves = 1 << self.depth;
        let mut curves =
            LinearConfig::new(self.hidden, nodes * self.control_points * 2).init(device);
        curves.weight = curves.weight.map(|w| w * 0.1);
        curves.bias = Some(Param::from_tensor(Tensor::from_data(
            TensorData::new(
                straight_cuts(nodes, self.control_points),
                [nodes * self.control_points * 2],
            ),
            device,
        )));

        TreeNet {
            down,
            pool: MaxPool2dConfig::new([2, 2]).with_strides([2, 2]).init(),
            hidden: LinearConfig::new(flat, self.hidden).init(device),
            curves,
            leaves: LinearConfig::new(self.hidden, leaves * (self.classes + 1)).init(device),
            relu: Relu::new(),
            classes: self.classes,
            depth: self.depth,
            control_points: self.control_points,
            samples: self.samples,
            size: self.size,
        }
    }
}

/// The raw values of `nodes` straight cuts of `points` control points, as the
/// head's bias.
fn straight_cuts(nodes: usize, points: usize) -> Vec<f32> {
    let mut raw = Vec::with_capacity(nodes * points * 2);
    for node in 0..nodes {
        let angle = node as f32 * 2.399_963;
        let (dy, dx) = angle.sin_cos();
        for k in 0..points {
            let along = -0.45 + 0.9 * k as f32 / (points - 1).max(1) as f32;
            for offset in [dx * along, dy * along] {
                raw.push((offset / 0.75).atanh());
            }
        }
    }
    raw
}

/// What a forward pass says.
#[derive(Clone, Debug)]
pub struct TreeAnswer<B: Backend> {
    /// The control points of every cut, `[batch, 2^depth - 1, points, 2]`,
    /// the root first and level by level after it.
    pub curves: Tensor<B, 4>,
    /// The logits of every leaf, `[batch, 2^depth, classes + 1]`.
    pub leaves: Tensor<B, 3>,
}

impl<B: Backend> TreeNet<B> {
    /// Reads a picture, `[batch, 3, size, size]`.
    pub fn forward(&self, image: Tensor<B, 4>) -> TreeAnswer<B> {
        let [batch, ..] = image.dims();
        let mut x = image;
        for block in &self.down {
            x = self.pool.forward(block.forward(x));
        }
        let x: Tensor<B, 2> = self.relu.forward(self.hidden.forward(x.flatten::<2>(1, 3)));

        let nodes = (1 << self.depth) - 1;
        let raw = self.curves.forward(x.clone());
        let curves = raw.tanh().mul_scalar(0.75).add_scalar(0.5).reshape([
            batch,
            nodes,
            self.control_points,
            2,
        ]);
        let leaves = self
            .leaves
            .forward(x)
            .reshape([batch, 1 << self.depth, self.classes + 1]);
        TreeAnswer { curves, leaves }
    }

    /// The share of every pixel each leaf has, `[batch, leaves, size * size]`,
    /// pixels row by row. They sum to one over the leaves.
    ///
    /// `temperature` is in picture widths: `0.05` blurs a cut over a twentieth
    /// of the picture.
    pub fn partition(&self, curves: Tensor<B, 4>, temperature: f32) -> Tensor<B, 3> {
        let device = curves.device();
        let [batch, ..] = curves.dims();
        let pixels = pixel_centers::<B>(self.size, &device);
        let area = self.size * self.size;
        let basis = bernstein::<B>(self.control_points, self.samples, &device);

        let mut share = Tensor::<B, 3>::ones([batch, 1, area], &device);
        for level in 0..self.depth {
            let first = (1 << level) - 1;
            let count = 1 << level;
            let cuts = curves.clone().slice([
                0..batch,
                first..first + count,
                0..self.control_points,
                0..2,
            ]);
            let s = signed_distance(cuts, basis.clone(), pixels.clone());
            let side = sigmoid(s.div_scalar(temperature));
            let a = share.clone() * side.clone();
            let b = share * side.neg().add_scalar(1.0);
            // Interleave: the children of node i are 2i and 2i + 1.
            share = Tensor::stack::<4>(vec![a, b], 2).reshape([batch, count * 2, area]);
        }
        share
    }

    /// The class distribution of every pixel, `[batch, classes + 1, size,
    /// size]`, out of an answer.
    pub fn probabilities(&self, answer: &TreeAnswer<B>, temperature: f32) -> Tensor<B, 4> {
        let [batch, leaves, classes] = answer.leaves.dims();
        let share = self.partition(answer.curves.clone(), temperature);
        // [B, C, L] x [B, L, P] -> [B, C, P], the leaves' own softmax mixed
        // by how much of the pixel each has.
        let symbols = softmax(answer.leaves.clone(), 2).swap_dims(1, 2);
        debug_assert_eq!(symbols.dims(), [batch, classes, leaves]);
        symbols
            .matmul(share)
            .reshape([batch, classes, self.size, self.size])
    }

    /// The negative log-likelihood of the label, `class` being
    /// `[batch, size, size]` with the frame as `classes`.
    pub fn loss(
        &self,
        image: Tensor<B, 4>,
        class: Tensor<B, 3, Int>,
        temperature: f32,
    ) -> Tensor<B, 1> {
        let answer = self.forward(image);
        let probabilities = self.probabilities(&answer, temperature);
        let [batch, _, height, width] = probabilities.dims();
        let target = class.reshape([batch, 1, height, width]);
        probabilities
            .gather(1, target)
            .add_scalar(1e-6)
            .log()
            .neg()
            .mean()
    }

    /// How many leaves the tree has.
    pub fn leaves(&self) -> usize {
        1 << self.depth
    }

    /// Cuts from the root to a leaf.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The side of the pictures it was built for, in pixels.
    pub fn size(&self) -> usize {
        self.size
    }

    /// How many opaque areas it tells apart, not counting the frame.
    pub fn classes(&self) -> usize {
        self.classes
    }

    /// The class each leaf of `answer` says, picture 0 of the batch: the
    /// likeliest of its logits, the frame being [`TreeNet::classes`].
    pub fn leaf_classes(&self, answer: &TreeAnswer<B>) -> Vec<usize> {
        let [_, leaves, _] = answer.leaves.dims();
        answer
            .leaves
            .clone()
            .slice([0..1, 0..leaves, 0..self.classes + 1])
            .argmax(2)
            .into_data()
            .convert::<i64>()
            .to_vec::<i64>()
            .unwrap_or_default()
            .into_iter()
            .map(|class| class as usize)
            .collect()
    }

    /// The leaf every pixel of a `width` by `height` picture is in, row by
    /// row, with the cuts of picture 0 of `curves` made **hard**: a pixel
    /// goes to the first half of a cut where its signed distance is above
    /// nought and to the second where it is not, which is what the soft
    /// share becomes as the temperature goes to nothing.
    ///
    /// The curves are in picture coordinates, so the picture asked about need
    /// not be the size the network saw: a cut is as sharp at 1650 pixels as
    /// at 64, which is what makes the answer a vector one. Worked in chunks
    /// of pixels, since the distance to every sample of every cut of a level
    /// is a tensor as long as the picture is large.
    pub fn leaf_map(&self, curves: Tensor<B, 4>, width: usize, height: usize) -> Vec<u32> {
        const CHUNK: usize = 16_384;
        let device = curves.device();
        let basis = bernstein::<B>(self.control_points, self.samples, &device);
        let total = width * height;
        let mut leaves = Vec::with_capacity(total);
        let mut start = 0;
        while start < total {
            let end = (start + CHUNK).min(total);
            let pixels = pixel_centers_of::<B>(width, height, start..end, &device);
            let count = end - start;
            // Where each pixel of the chunk is in the level it has come to:
            // its place among that level's nodes.
            let mut node = Tensor::<B, 3, Int>::zeros([1, 1, count], &device);
            for level in 0..self.depth {
                let first = (1 << level) - 1;
                let nodes = 1 << level;
                let cuts = curves.clone().slice([
                    0..1,
                    first..first + nodes,
                    0..self.control_points,
                    0..2,
                ]);
                let s = signed_distance(cuts, basis.clone(), pixels.clone());
                let here = s.gather(1, node.clone());
                let second = here.lower_equal_elem(0.0).int();
                node = node * 2 + second;
            }
            let chunk: Vec<i64> = node
                .into_data()
                .convert::<i64>()
                .to_vec::<i64>()
                .unwrap_or_default();
            leaves.extend(chunk.into_iter().map(|leaf| leaf as u32));
            start = end;
        }
        leaves
    }
}

/// The centre of every pixel of a `size` square in picture coordinates,
/// `[1, 1, size * size, 1, 2]` (x, then y), row by row.
fn pixel_centers<B: Backend>(size: usize, device: &B::Device) -> Tensor<B, 5> {
    pixel_centers_of(size, size, 0..size * size, device)
}

/// The centres of pixels `range` (counted row by row) of a `width` by
/// `height` picture, in picture coordinates: `[1, 1, range.len(), 1, 2]`,
/// x then y, both across `[0, 1]` whatever the picture's shape.
fn pixel_centers_of<B: Backend>(
    width: usize,
    height: usize,
    range: std::ops::Range<usize>,
    device: &B::Device,
) -> Tensor<B, 5> {
    let count = range.len();
    let mut xy = Vec::with_capacity(count * 2);
    for at in range {
        xy.push(((at % width) as f32 + 0.5) / width as f32);
        xy.push(((at / width) as f32 + 0.5) / height as f32);
    }
    Tensor::from_data(TensorData::new(xy, [1, 1, count, 1, 2]), device)
}

/// The Bernstein basis, `[1, 1, samples, points, 1]`: row `m` weighs each
/// control point at the `m`-th of `samples` evenly spaced parameters.
fn bernstein<B: Backend>(points: usize, samples: usize, device: &B::Device) -> Tensor<B, 5> {
    let degree = points - 1;
    let mut basis = Vec::with_capacity(samples * points);
    for m in 0..samples {
        let t = m as f32 / (samples - 1).max(1) as f32;
        let mut choose = 1.0_f32;
        for k in 0..points {
            if k > 0 {
                choose = choose * (degree + 1 - k) as f32 / k as f32;
            }
            basis.push(choose * t.powi(k as i32) * (1.0 - t).powi((degree - k) as i32));
        }
    }
    Tensor::from_data(TensorData::new(basis, [1, 1, samples, points, 1]), device)
}

/// The signed distance from every pixel to each of a level's curves,
/// `[batch, nodes, pixels]`: positive on the left of the way the curve runs,
/// negative on its right.
///
/// `cuts` is `[batch, nodes, points, 2]`, `basis` what [`bernstein`] built
/// and `pixels` what [`pixel_centers`] did. The curve is sampled, continued
/// by a straight ray past each end, and the distance is the least to any of
/// the resulting segments; the sign is that of the cross product with the
/// segment it is nearest to.
fn signed_distance<B: Backend>(
    cuts: Tensor<B, 4>,
    basis: Tensor<B, 5>,
    pixels: Tensor<B, 5>,
) -> Tensor<B, 3> {
    let [batch, nodes, points, _] = cuts.dims();
    let [.., samples, _, _] = basis.dims();
    let [.., area, _, _] = pixels.dims();

    // [B, N, 1, K, 2] * [1, 1, M, K, 1], summed over K — a broadcast
    // multiply-add, not a matmul, so the positions stay exact on a GPU.
    let sampled = (cuts.reshape([batch, nodes, 1, points, 2]) * basis)
        .sum_dim(3)
        .reshape([batch, nodes, samples, 2]);

    // Continue both ends along their own direction.
    let head = sampled.clone().slice([0..batch, 0..nodes, 0..1, 0..2]);
    let second = sampled.clone().slice([0..batch, 0..nodes, 1..2, 0..2]);
    let last = sampled
        .clone()
        .slice([0..batch, 0..nodes, samples - 1..samples, 0..2]);
    let before = sampled
        .clone()
        .slice([0..batch, 0..nodes, samples - 2..samples - 1, 0..2]);
    let toward_start = unit(head.clone() - second);
    let toward_end = unit(last.clone() - before);
    let line = Tensor::cat(
        vec![
            head + toward_start.mul_scalar(EXTENSION),
            sampled,
            last + toward_end.mul_scalar(EXTENSION),
        ],
        2,
    );
    let segments = samples + 1;

    let a = line
        .clone()
        .slice([0..batch, 0..nodes, 0..segments, 0..2])
        .reshape([batch, nodes, 1, segments, 2]);
    let b = line
        .clone()
        .slice([0..batch, 0..nodes, 1..segments + 1, 0..2])
        .reshape([batch, nodes, 1, segments, 2]);
    let ab = b - a.clone();
    let ap = pixels.clone() - a; // [B, N, P, S, 2]

    let ab_x = ab
        .clone()
        .slice([0..batch, 0..nodes, 0..1, 0..segments, 0..1]);
    let ab_y = ab.slice([0..batch, 0..nodes, 0..1, 0..segments, 1..2]);
    let ap_x = ap
        .clone()
        .slice([0..batch, 0..nodes, 0..area, 0..segments, 0..1]);
    let ap_y = ap.slice([0..batch, 0..nodes, 0..area, 0..segments, 1..2]);

    let along = (ap_x.clone() * ab_x.clone() + ap_y.clone() * ab_y.clone())
        / (ab_x.clone() * ab_x.clone() + ab_y.clone() * ab_y.clone()).add_scalar(1e-9);
    let along = along.clamp(0.0, 1.0);
    let off_x = ap_x.clone() - along.clone() * ab_x;
    let off_y = ap_y.clone() - along * ab_y;
    let squared =
        (off_x.clone() * off_x + off_y.clone() * off_y).reshape([batch, nodes, area, segments]);
    let distance = squared
        .min_dim(3)
        .add_scalar(1e-12)
        .sqrt()
        .reshape([batch, nodes, area]);

    // The side: whether the pixel is inside the polygon the cut and its far
    // side make, by the parity of how many times a ray to the right crosses
    // it. Unlike the sign of the nearest segment, which flips along the
    // medial axis of a curve which curls — where there is no cut at all — this
    // flips only on the cut, so the regions the network trains on are the
    // regions the polygons of `crate::tree_vector` are.
    let inside = inside_polygon(line.detach(), pixels, batch, nodes, area);
    distance * inside.mul_scalar(2.0).sub_scalar(1.0)
}

/// `1` where a pixel is inside the closed polygon a cut makes with its far
/// side, `0` where it is not: `[batch, nodes, pixels]`.
///
/// `line` is the cut with its two extensions, `[batch, nodes, points, 2]`;
/// the polygon is that, then a point far off on the positive side of each end
/// — positive being `(-dy, dx)` from the way the cut runs there — and back.
/// A pixel is inside where a ray to the right of it crosses the polygon an odd
/// number of times, which is the even-odd rule.
fn inside_polygon<B: Backend>(
    line: Tensor<B, 4>,
    pixels: Tensor<B, 5>,
    batch: usize,
    nodes: usize,
    area: usize,
) -> Tensor<B, 3> {
    let points = line.dims()[2];
    let at = |i: usize| line.clone().slice([0..batch, 0..nodes, i..i + 1, 0..2]);
    let normal = |v: Tensor<B, 4>| {
        let dx = v.clone().slice([0..batch, 0..nodes, 0..1, 0..1]);
        let dy = v.slice([0..batch, 0..nodes, 0..1, 1..2]);
        let length = (dx.clone() * dx.clone() + dy.clone() * dy.clone())
            .add_scalar(1e-12)
            .sqrt();
        Tensor::cat(vec![dy.neg() / length.clone(), dx / length], 3)
    };
    let (first, last) = (at(0), at(points - 1));
    let n_first = normal(at(1) - first.clone());
    let n_last = normal(last.clone() - at(points - 2));
    let ring = Tensor::cat(
        vec![
            line.clone(),
            last + n_last.mul_scalar(FAR),
            first.clone() + n_first.mul_scalar(FAR),
        ],
        2,
    );
    let edges = points + 2;
    let a = ring.clone();
    let b = Tensor::cat(
        vec![ring.slice([0..batch, 0..nodes, 1..edges, 0..2]), first],
        2,
    );
    let column = |t: &Tensor<B, 4>, c: usize| {
        t.clone()
            .slice([0..batch, 0..nodes, 0..edges, c..c + 1])
            .reshape([batch, nodes, 1, edges])
    };
    let (a_x, a_y, b_x, b_y) = (column(&a, 0), column(&a, 1), column(&b, 0), column(&b, 1));
    let px = pixels
        .clone()
        .slice([0..1, 0..1, 0..area, 0..1, 0..1])
        .reshape([1, 1, area, 1]);
    let py = pixels
        .slice([0..1, 0..1, 0..area, 0..1, 1..2])
        .reshape([1, 1, area, 1]);

    // An edge is crossed by the ray where its ends are on either side of the
    // pixel's row and the edge is to the pixel's right at that row.
    // Compared at the full shape: a comparison, unlike arithmetic, does not
    // stretch a dimension of one on every backend.
    let full = [batch, nodes, area, edges];
    let py_full = py.clone().expand(full);
    let above_a = a_y.clone().expand(full).greater(py_full.clone()).float();
    let above_b = b_y.clone().expand(full).greater(py_full).float();
    let straddles = (above_a - above_b).abs();
    let x_at = a_x.clone() + (py - a_y.clone()) / (b_y - a_y) * (b_x - a_x);
    let to_the_right = x_at.greater(px.expand(full)).float();
    let crossings = (straddles * to_the_right).sum_dim(3);
    let parity = crossings.clone() - crossings.div_scalar(2.0).floor().mul_scalar(2.0);
    parity.reshape([batch, nodes, area])
}

/// A vector `[.., 2]` scaled to length one.
fn unit<B: Backend>(v: Tensor<B, 4>) -> Tensor<B, 4> {
    let length = v
        .clone()
        .powf_scalar(2.0)
        .sum_dim(3)
        .add_scalar(1e-12)
        .sqrt();
    v / length
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn::backend::ndarray::{NdArray, NdArrayDevice};
    use burn::backend::Autodiff;
    use burn::module::AutodiffModule;
    use burn::optim::{AdamConfig, GradientsParams, Optimizer};

    type Plain = NdArray<f32>;
    type Diff = Autodiff<Plain>;

    const SIZE: usize = 32;

    /// A picture cut twice by parallel lines at an angle: three bands, three
    /// classes, three colours. The kind of thing two cuts of the tree draw.
    fn bands(angle: f32, first: f32, second: f32) -> (Vec<f32>, Vec<i64>) {
        let colours = [[0.9, 0.2, 0.2], [0.2, 0.9, 0.2], [0.2, 0.2, 0.9]];
        let (s, c) = angle.sin_cos();
        let mut image = vec![0.0; 3 * SIZE * SIZE];
        let mut class = vec![0; SIZE * SIZE];
        for row in 0..SIZE {
            for column in 0..SIZE {
                let x = (column as f32 + 0.5) / SIZE as f32 - 0.5;
                let y = (row as f32 + 0.5) / SIZE as f32 - 0.5;
                let d = x * c + y * s;
                let k = if d < first {
                    0
                } else if d < second {
                    1
                } else {
                    2
                };
                class[row * SIZE + column] = k as i64;
                for channel in 0..3 {
                    image[channel * SIZE * SIZE + row * SIZE + column] = colours[k][channel];
                }
            }
        }
        (image, class)
    }

    #[test]
    fn the_leaves_tile_the_picture() {
        let device = NdArrayDevice::default();
        let net = TreeNetConfig::new(2)
            .with_size(SIZE)
            .with_depth(3)
            .init::<Plain>(&device);
        let answer = net.forward(Tensor::zeros([2, 3, SIZE, SIZE], &device));
        assert_eq!(answer.curves.dims(), [2, 7, 6, 2]);
        assert_eq!(answer.leaves.dims(), [2, 8, 3]);

        let share = net.partition(answer.curves.clone(), 0.05);
        assert_eq!(share.dims(), [2, 8, SIZE * SIZE]);
        let total = share.sum_dim(1);
        let worst: f32 = (total - 1.0).abs().max().into_scalar();
        assert!(worst < 1e-4, "shares sum to one, off by {worst}");

        let probabilities = net.probabilities(&answer, 0.05);
        assert_eq!(probabilities.dims(), [2, 3, SIZE, SIZE]);
        let worst: f32 = (probabilities.sum_dim(1) - 1.0).abs().max().into_scalar();
        assert!(worst < 1e-4, "probabilities sum to one, off by {worst}");
    }

    /// What the whole of the vector export stands on: for cuts which curl and
    /// cross themselves, the polygons `tree_vector` makes are the regions the
    /// network assigns its pixels to. Pixel by pixel, over several random
    /// trees, with every leaf a class of its own so a leaf can be told.
    #[test]
    fn the_polygons_of_curled_cuts_are_the_regions_the_net_assigns() {
        use crate::tree_vector::CutTree;
        use geo::Contains;

        let device = NdArrayDevice::default();
        let (depth, points, samples, side) = (3, 8, 24, 48);
        let net = TreeNetConfig::new(8)
            .with_size(SIZE)
            .with_depth(depth)
            .with_control_points(points)
            .with_samples(samples)
            .init::<Plain>(&device);
        let nodes = (1 << depth) - 1;

        for seed in 1..=6u64 {
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut random = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64
            };
            // Control points anywhere from well outside the picture to well
            // inside: curls, loops and cuts which cross themselves.
            let values: Vec<f64> = (0..nodes * points * 2)
                .map(|_| -0.3 + 1.6 * random())
                .collect();
            let curves = Tensor::<Plain, 4>::from_data(
                TensorData::new(
                    values.iter().map(|&v| v as f32).collect::<Vec<_>>(),
                    [1, nodes, points, 2],
                ),
                &device,
            );
            let leaves = net.leaf_map(curves, side, side);

            let tree = CutTree {
                depth,
                cuts: (0..nodes)
                    .map(|n| {
                        (0..points)
                            .map(|k| {
                                (
                                    values[(n * points + k) * 2],
                                    values[(n * points + k) * 2 + 1],
                                )
                            })
                            .collect()
                    })
                    .collect(),
                leaf_class: (0..8).collect(),
            };
            let regions = tree.regions(samples, 8, 0.0);

            let mut agree = 0;
            for (at, &leaf) in leaves.iter().enumerate() {
                let p = geo::Point::new(
                    ((at % side) as f64 + 0.5) / side as f64,
                    ((at / side) as f64 + 0.5) / side as f64,
                );
                let found = regions.iter().find(|(_, shape)| shape.contains(&p));
                // A pixel on no polygon is one the sliver filter dropped.
                if found.is_some_and(|(class, _)| *class == leaf as usize) {
                    agree += 1;
                }
            }
            let share = agree as f64 / leaves.len() as f64;
            assert!(
                share > 0.995,
                "seed {seed}: only {:.2}% agree",
                share * 100.0
            );
        }
    }

    /// The hard tree and the soft one say the same where the cuts are far
    /// away, and the hard one can be asked at any size.
    #[test]
    fn the_hard_tree_assigns_every_pixel_a_leaf_at_any_size() {
        let device = NdArrayDevice::default();
        let net = TreeNetConfig::new(2)
            .with_size(SIZE)
            .with_depth(3)
            .init::<Plain>(&device);
        let answer = net.forward(Tensor::zeros([1, 3, SIZE, SIZE], &device));
        let leaves = net.leaf_map(answer.curves.clone(), SIZE, SIZE);
        assert_eq!(leaves.len(), SIZE * SIZE);
        assert!(leaves.iter().all(|&leaf| leaf < 8));

        // The leaf with the largest share at a pixel is its leaf wherever the
        // cuts are not blurred together: at a cold temperature nearly all.
        let share = net.partition(answer.curves.clone(), 0.002);
        let best: Vec<i64> = share
            .argmax(1)
            .into_data()
            .convert::<i64>()
            .to_vec()
            .unwrap();
        let agree = leaves
            .iter()
            .zip(&best)
            .filter(|(hard, soft)| **hard as i64 == **soft)
            .count();
        assert!(
            agree * 100 >= leaves.len() * 98,
            "{agree} of {}",
            leaves.len()
        );

        // A picture twice the size, not square: a leaf for every pixel still.
        let bigger = net.leaf_map(answer.curves.clone(), 2 * SIZE, 3 * SIZE);
        assert_eq!(bigger.len(), 2 * SIZE * 3 * SIZE);
        assert_eq!(net.leaf_classes(&answer).len(), 8);
    }

    /// The distance is a distance and the sign is a side.
    #[test]
    fn a_straight_cut_is_a_signed_distance() {
        let device = NdArrayDevice::default();
        // The horizontal line y = 0.5, run left to right.
        let cuts = Tensor::<Plain, 4>::from_data(
            TensorData::new(vec![0.1, 0.5, 0.9, 0.5], [1, 1, 2, 2]),
            &device,
        );
        let s = signed_distance(
            cuts,
            bernstein::<Plain>(2, 8, &device),
            pixel_centers::<Plain>(4, &device),
        );
        let s: Vec<f32> = s.into_data().to_vec().unwrap();
        // Pixel centres are 0.125, 0.375, 0.625, 0.875 down; row 0 is 0.375
        // above the line and row 3 is 0.375 below it, and they differ in sign.
        assert!((s[0].abs() - 0.375).abs() < 1e-4, "{}", s[0]);
        assert!(s[0] * s[3 * 4] < 0.0);
        assert!((s[0] + s[3 * 4 + 1]).abs() < 0.8);
    }

    /// The question the whole idea turns on: does a gradient through the
    /// soft tree find the cuts. Four pictures, remembered.
    #[test]
    #[ignore = "two minutes in release and hours in debug: cargo test --release -- --ignored"]
    fn it_learns_to_cut_four_pictures() {
        let device = NdArrayDevice::default();
        let mut net = TreeNetConfig::new(3)
            .with_size(SIZE)
            .with_depth(3)
            .init::<Diff>(&device);

        let specs = [
            (0.0_f32, -0.15, 0.15),
            (1.2, -0.2, 0.1),
            (2.2, -0.1, 0.2),
            (3.6, -0.18, 0.12),
        ];
        let mut images = Vec::new();
        let mut classes = Vec::new();
        for (angle, a, b) in specs {
            let (image, class) = bands(angle, a, b);
            images.extend(image);
            classes.extend(class);
        }
        let image =
            Tensor::<Diff, 4>::from_data(TensorData::new(images, [4, 3, SIZE, SIZE]), &device);
        let class =
            Tensor::<Diff, 3, Int>::from_data(TensorData::new(classes, [4, SIZE, SIZE]), &device);

        let mut optimizer = AdamConfig::new().init::<Diff, TreeNet<Diff>>();
        let mut first = 0.0;
        let mut last = 0.0;
        for step in 0..400 {
            let loss = net.loss(image.clone(), class.clone(), 0.05);
            let value: f32 = loss.clone().into_scalar();
            if step == 0 {
                first = value;
            }
            last = value;
            let grads = GradientsParams::from_grads(loss.backward(), &net);
            net = optimizer.step(2e-3, net, grads);
        }
        eprintln!("loss {first:.3} -> {last:.3}");

        let valid = net.valid();
        let answer = valid.forward(image.clone().inner());
        let probabilities = valid.probabilities(&answer, 0.05);
        let predicted = probabilities.argmax(1).reshape([4, SIZE, SIZE]);
        let truth = class.inner();
        let right: f32 =
            predicted.equal(truth).int().sum().into_scalar() as f32 / (4 * SIZE * SIZE) as f32;
        eprintln!("pixel accuracy {right:.3}");
        assert!(last < first * 0.5, "{first} -> {last}");
        assert!(right > 0.8, "accuracy {right}");
    }
}
