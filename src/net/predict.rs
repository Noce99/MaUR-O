//! Running a trained network over a whole picture.
//!
//! [`crate::net::training`] leaves a folder behind; this is what reads one back
//! and puts a map through it. Two things stand between the weights and an
//! answer about a picture, and this module is both of them.
//!
//! # The weights
//!
//! A record is tensors and nothing else — it does not know how many symbols
//! the network was told apart or how wide its first level was, and a module
//! has to be built to that shape before a record can be loaded into it. Both
//! numbers are in the `training.json` the run wrote before it started, which
//! is why [`load`] reads that first and the weights second.
//!
//! The weights are a burnpack file (`.bpk`), burn's own record format, written
//! at the precision the module holds, which is `f32`. That matters for one
//! reason: a batch normalization's running variance does not fit in an `f16`.
//! This network's reach 1e5 by the fourth level and `f16` tops out at 65504,
//! so a file written at half precision stores those channels as infinities,
//! and a channel loaded with an infinite variance normalizes to nothing and
//! emits its own bias everywhere. [`load`] counts such channels and says so.
//!
//! `best.bpk` is the epoch which validated best; a `checkpoint/007/model.bpk`
//! is the same format and loads the same way, which is what `weights` is for.
//!
//! # The picture
//!
//! A U-Net of [`DEPTH`] levels halves what it is given four times over, so
//! its input has to divide by sixteen — and a dataset image is 1650 pixels
//! square, which does not. It is also two and a half million pixels, against
//! the 256 by 256 crops the network was trained on.
//!
//! So a picture is not put through in one piece. It is cut into tiles the
//! size the run trained at, overlapping by a quarter of that, and each tile
//! is predicted on its own. Every pixel is covered by at least one tile and
//! usually by two or four, and the answer kept for it is **the one from the
//! tile which saw it furthest from that tile's own edge** — which is the tile
//! that had the most of its surroundings to go on. That is what keeps a seam
//! from appearing along the tile boundaries: near the join, the tile whose
//! middle a pixel is nearer wins, and no pixel is ever answered for by a tile
//! which only just caught it.
//!
//! Averaging the tiles instead would want a `classes + 1` stack of floats per
//! pixel to average into, which for a full map and a real symbol set is a
//! third of a gigabyte. Picking wants a class, an angle and a distance, which
//! is eight bytes.
//!
//! A picture smaller than one tile is padded out to a multiple of sixteen
//! with white — the colour of the frame around a map, which is what the
//! network was taught to call background — and the answer is cropped back to
//! the size it came in at.
//!
//! # What comes out
//!
//! A [`SymbolGrid`], which is what [`crate::vectorize`] turns into a map.
//! The class of a pixel is the argmax over the `classes + 1` logits of
//! [`UNet::forward`], with a win for the frame becoming [`BACKGROUND`]; the
//! angle is `atan2(sin, cos)` of the last two channels, and [`NO_ROTATION`]
//! where the pair is shorter than half a unit — the same threshold
//! [`crate::net::training`] masks its rotation score with, and for the same
//! reason: a label is either the zero vector or a point on the unit circle,
//! so anything in between is the network saying it does not know.
//!
//! What it does **not** do is decide that a symbol with no pattern to turn
//! has no angle. That needs the symbol set, which is a thing about the map
//! rather than about the network — see [`resolve_angles`], which is also
//! where the angle is unfolded by the symbol's own symmetry.
//!
//! # How dense the picture is
//!
//! A network reads best at the densities its dataset was drawn at, and the
//! map it writes is only the right size if the density of the picture is
//! known. [`ReadBackSettings::resolution`] is the picture's density and
//! [`ReadBackSettings::network_resolutions`] the range the network was
//! trained over — one density, for a dataset drawn at one. A picture inside
//! the range is read as it is, every pixel of it; one outside is resampled to
//! the nearer end of it ([`ReadBackSettings::reading_resolution`]) and no
//! further, so a 600 dpi scan read by a network trained up to 400 dpi loses
//! a third of its pixels rather than all but a quarter. [`read_back`] then
//! places what the network said over the ground
//! the *original* picture covers, and draws the map back at the original
//! density to score it — so the drawing is the picture's own size whatever
//! was resampled on the way.
//!
//! Where the picture's density is not known, a network with a scale head can
//! say it: [`estimate_resolution`] asks it about tiles of the picture and
//! takes the median.
//!
//! # The whole way round
//!
//! [`read_back`] is all of it in one call: a picture in, the map the network
//! read out of it written to a file, that map drawn again, and the drawing
//! measured against the picture it came from. The `mauro net image-to-map` tool is that
//! function with a command line around it, and [`crate::net::image_valid`] is
//! that function once per epoch of a run — which is the point of it being one
//! function. A sheet a run leaves behind has to show what the tool would
//! give, or it is a picture of something else.

use std::path::Path;

use burn::module::Module;
use burn::store::ModuleRecord;
use burn::tensor::{Device, Tensor, TensorData};
use image::RgbImage;

use crate::differences::{difference_mask, Comparison, DEFAULT_TOLERANCE};
use crate::geometry::Rect;
use crate::ground_truth::{BACKGROUND, NO_ROTATION};
use crate::line_vector::{LineGrid, LineSettings};
use crate::render::{render_map_over, to_rgb_image, Extent};
use crate::symbol_kinds::Entry;
use crate::vectorize::{
    objects_of_regions, write_map_with_lines, write_objects, Placement, Simplify, SymbolGrid,
    Written,
};

use crate::net::training::TrainingConfig;
use crate::net::tree::{TreeAnswer, TreeNet};
use crate::net::unet::{UNet, UNetConfig, DEPTH};
use crate::tree_vector::CutTree;

/// The weights of the epoch which validated best, which is what a run folder
/// holds under this name.
pub const BEST_WEIGHTS: &str = "best";

/// How much of a tile overlaps the one before it, as a share of its size.
///
/// A quarter each way, so a pixel in the middle half of a tile is answered
/// for by that tile and a pixel near its edge is answered for by the
/// neighbour whose middle it is nearer.
pub const OVERLAP: f64 = 0.25;

/// How long the predicted angle vector has to be before it is an angle at
/// all, rather than the network declining to give one.
///
/// A label is either the zero vector or exactly on the unit circle, so half a
/// unit is the line between them. `training::agreement` masks its score with
/// the same number.
pub const ANGLE_LENGTH: f32 = 0.5;

/// The model a run folder holds, and the configuration it was built to.
///
/// `weights` names the record to load, and is `<run>/best.bpk` where it is
/// `None`. A checkpoint — `<run>/checkpoint/007/model.bpk` — is the same
/// format and may be named instead, which is how a run is looked at as it was
/// part way through.
pub fn load(
    run: &Path,
    weights: Option<&Path>,
    device: &Device,
) -> Result<(UNet, TrainingConfig), String> {
    let configuration = run.join("training.json");
    if !configuration.exists() && run.join("tree.json").exists() {
        return Err(format!(
            "{} is a run of the tree network (it has a tree.json, not a training.json), and this \
             loads a U-Net: see `tree_training::load_tree` and `predict_tree_image`.",
            run.display(),
        ));
    }
    // The shape of the module has to be known before a record can be loaded
    // into it, and a record does not carry it.
    let config = TrainingConfig::read(&configuration).map_err(|e| {
        format!(
            "cannot read {}: {e}\nA run folder is what `train` wrote: its training.json is what \
             says how big the network it trained was.",
            configuration.display(),
        )
    })?;

    let default = run.join(BEST_WEIGHTS);
    let weights = weights.unwrap_or(&default);
    let model = UNetConfig::new(config.classes)
        .with_base_channels(config.base_channels)
        .with_lines(config.lines)
        .with_scale_head(config.scale_head)
        .init(device);
    let model = load_weights(model, weights)?;

    // A file written at half precision. It opens, but a variance past 65504
    // was stored as an infinity, and a batch normalization channel loaded
    // with one emits its own bias and nothing else. Said out loud, because
    // the answer looks like a network which learned the wrong thing rather
    // than like a file which lost what it learned.
    let dead = model.constant_channels();
    if dead > 0 {
        eprintln!(
            "Warning: {} holds {dead} batch normalization channels whose variance is not finite. \
             It was written at half precision: every variance over 65504 was stored as an \
             infinity, and a channel loaded with one is a constant. What this model says is worth no more than the file. Train again.",
            weights.display(),
        );
    }
    Ok((model, config))
}

/// Loads the weights in `weights` into `module`, which has to be built to the
/// shape they were written in.
///
/// `weights` is the file as it was written: the `.bpk` of a [`ModuleRecord`],
/// burn's burnpack format. A name without an extension is given one, which is
/// how [`BEST_WEIGHTS`] is passed.
pub fn load_weights<M: Module>(module: M, weights: &Path) -> Result<M, String> {
    let weights = weights_file(weights);
    let record = ModuleRecord::load(&weights).map_err(|e| {
        format!(
            "cannot read the weights from {}: {e}\nA run which was stopped before it \
             finished an epoch has none: there is no best.bpk and checkpoint/ is empty.",
            weights.display(),
        )
    })?;
    module.try_load_record(record).map_err(|e| {
        format!(
            "the weights in {} do not fit the network: {e}",
            weights.display(),
        )
    })
}

/// Saves `module`'s weights to `weights`, giving it the `.bpk` extension
/// where it has none.
pub fn save_weights<M: Module>(module: M, weights: &Path) -> Result<(), String> {
    let weights = weights_file(weights);
    module
        .into_record()
        .save(&weights)
        .map_err(|e| format!("cannot write the weights to {}: {e}", weights.display()))
}

/// `path`, with the extension of a burnpack file where it has none.
fn weights_file(path: &Path) -> std::path::PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension("bpk")
    }
}

/// What the network says about a whole picture, tile by tile.
///
/// `tile` is how many pixels square a tile is — the crop the run trained at
/// is the size to hand it — and has to divide by `2^DEPTH`. `overlap` is how
/// much of each tile is shared with the one before it, in pixels.
///
/// Both are clamped rather than refused: a tile larger than the picture
/// becomes the picture, and an overlap which would leave the tiles standing
/// still becomes one that does not.
pub fn predict_image(
    model: &UNet,
    image: &RgbImage,
    tile: usize,
    overlap: usize,
    device: &Device,
) -> SymbolGrid {
    predict_image_with_lines(model, image, tile, overlap, DEFAULT_LINE_THRESHOLD, device).0
}

/// What a line is found at: the probability a pixel has to be given for a
/// line of a symbol to run through it. Half, which is where a sigmoid is
/// as sure of one answer as of the other.
pub const DEFAULT_LINE_THRESHOLD: f32 = 0.5;

/// [`predict_image`], and where the network says each line symbol runs.
///
/// The lines are the second layer of the head — see
/// [`crate::line_truth`] — and come out as a [`LineGrid`] when the network
/// has one, and as `None` when it does not. A line is found where its
/// probability is over `line_threshold`; the tiles are joined the way the
/// ground's are, the pixel taking the answer of the tile which saw it
/// furthest from its own edge.
pub fn predict_image_with_lines(
    model: &UNet,
    image: &RgbImage,
    tile: usize,
    overlap: usize,
    line_threshold: f32,
    device: &Device,
) -> (SymbolGrid, Option<LineGrid>) {
    let step = 1 << DEPTH;
    let (width, height) = (image.width() as usize, image.height() as usize);

    // A tile is no larger than the picture, and both are rounded up to what a
    // U-Net of DEPTH levels can halve. Rounding the picture up is what pads a
    // small one out; a picture larger than a tile is never padded, since the
    // last tile in each direction is slid back to end at its edge instead.
    let tile = tile.max(step).min(round_up(width.max(height), step));
    let tile = round_up(tile, step);
    let (padded_width, padded_height) = (
        if width < tile { tile } else { width },
        if height < tile { tile } else { height },
    );
    // Never so much overlap that the tiles stop advancing.
    let stride = tile.saturating_sub(overlap).max(1);

    let mut grid = SymbolGrid::new(width, height);
    let lines = model.lines();
    let mut line_grid = (lines > 0).then(|| LineGrid::new(width, height, lines));
    // A probability over the threshold is a logit over this, and comparing
    // the logit spares a sigmoid for every pixel of every channel.
    let line_logit = {
        let t = line_threshold.clamp(1e-6, 1.0 - 1e-6);
        (t / (1.0 - t)).ln()
    };
    // How far from its tile's edge each pixel's answer was found, and one
    // more than that so nought can mean no answer yet. A later tile only
    // overwrites a pixel it saw better than the tile before it did.
    let mut margin = vec![0u32; width * height];

    for top in starts(padded_height, tile, stride) {
        for left in starts(padded_width, tile, stride) {
            let input = tile_tensor(image, left, top, tile, device);
            // `classes + 1` class logits, the frame last, then the sine and
            // the cosine of the angle, then a logit for each line symbol.
            let raw = model.forward(input);
            let values = raw
                .into_data()
                .try_into_vec::<f32>()
                .expect("the head is a tensor of floats");
            let classes = model.classes();
            let pixels = tile * tile;

            for row in 0..tile {
                let y = top + row;
                if y >= height {
                    break;
                }
                for column in 0..tile {
                    let x = left + column;
                    if x >= width {
                        break;
                    }
                    // How much of its surroundings this tile had for this
                    // pixel: the distance to the nearest edge of the tile,
                    // and one more so that nought means no answer yet.
                    let reach =
                        (column.min(tile - 1 - column)).min(row.min(tile - 1 - row)) as u32 + 1;
                    let at = y * width + x;
                    // Strictly better, so the first tile to see a pixel keeps
                    // it where two saw it equally well.
                    if reach <= margin[at] {
                        continue;
                    }
                    margin[at] = reach;

                    let of = |channel: usize| values[channel * pixels + row * tile + column];
                    let winner = (0..=classes)
                        .max_by(|&a, &b| of(a).total_cmp(&of(b)))
                        .expect("a class or the frame");
                    grid.class[at] = if winner == classes {
                        BACKGROUND
                    } else {
                        winner as u16
                    };

                    // Not the last two channels: where there are lines, the
                    // last channels are theirs.
                    let (sin, cos) = (of(classes + 1), of(classes + 2));
                    grid.rotation[at] = if sin.hypot(cos) <= ANGLE_LENGTH {
                        // Not an angle it got wrong: an angle it declined to
                        // give, which is what the zero vector says.
                        NO_ROTATION
                    } else {
                        let turn = sin.atan2(cos) / std::f32::consts::TAU;
                        turn.rem_euclid(1.0)
                    };

                    if let Some(found) = &mut line_grid {
                        for line in 0..lines {
                            found.channel_mut(line)[at] =
                                (of(classes + 3 + line) > line_logit) as u8;
                        }
                    }
                }
            }
        }
    }
    (grid, line_grid)
}

/// What one picture came to, on the way through the network and back out as
/// a map.
pub struct ReadBack {
    /// The grid the network said, with the still symbols' angles taken off.
    pub grid: SymbolGrid,
    /// How many path objects the map came to, lines included.
    pub objects: usize,
    /// How many of them are lines, and how many line symbols those are drawn
    /// with: nought of both for a network with no lines layer.
    pub lines: usize,
    /// How many different line symbols the lines are drawn with.
    pub line_symbols: usize,
    /// How many coordinates those objects hold, control points included.
    pub coords: usize,
    /// The map drawn again, over the very ground the picture covers, so that
    /// it and the picture are the same size.
    pub drawn: RgbImage,
    /// How the two compare, pixel by pixel.
    pub comparison: Comparison,
    /// Complaints from drawing the map which was written.
    pub warnings: Vec<String>,
}

impl ReadBack {
    /// What share of the picture the network called the frame.
    ///
    /// A map which comes out with no objects at all is a network which called
    /// the whole picture the frame, and that is worth being able to say.
    pub fn frame_share(&self) -> f64 {
        let frame = self
            .grid
            .class
            .iter()
            .filter(|&&class| class == BACKGROUND)
            .count();
        frame as f64 / self.grid.class.len().max(1) as f64
    }

    /// How many of the symbol set's opaque areas it used on the rest.
    pub fn symbols_used(&self) -> usize {
        let mut used: Vec<u16> = self
            .grid
            .class
            .iter()
            .copied()
            .filter(|&class| class != BACKGROUND)
            .collect();
        used.sort_unstable();
        used.dedup();
        used.len()
    }

    /// What share of the pixels the drawing and the picture agree about, and
    /// the shares which differ as two renderers differ about an edge and
    /// which really differ.
    pub fn shares(&self) -> (f64, f64, f64) {
        let total = (self.comparison.mask.width as u64 * self.comparison.mask.height as u64).max(1);
        let real = self.comparison.mask.count_real();
        let edge = self.comparison.mask.count() - real;
        let share = |count: u64| count as f64 / total as f64;
        (share(total - edge - real), share(edge), share(real))
    }
}

/// A picture through the network, out as a map, and back to a picture to be
/// measured against the one it came from.
///
/// `into` is where the map is written, and it stays there — it is the answer,
/// and the drawing beside it is only how the answer is scored. The ground the
/// map covers is the picture's own size at `resolution`, centred on the
/// origin, which is exactly the square a dataset image covers and generalizes
/// to a picture of any size.
///
/// `symbol_set` has to be the file the network's classes are places in;
/// `symbols` its opaque areas, which is [`Catalogue::opaque_areas`](crate::symbol_kinds::Catalogue::opaque_areas) of the
/// same file.
#[allow(clippy::too_many_arguments)]
pub fn read_back(
    model: &UNet,
    picture: &RgbImage,
    symbol_set: &Path,
    symbols: &[Entry],
    into: &Path,
    settings: &ReadBackSettings,
    device: &Device,
) -> Result<ReadBack, String> {
    // The ground the picture covers, at its own density, whatever the
    // network is shown of it.
    let ground = ground_of(picture, settings);
    let resampled = resample(picture, settings.resolution, settings.reading_resolution());
    let (mut grid, lines) = predict_image_with_lines(
        model,
        resampled.as_ref().unwrap_or(picture),
        settings.tile,
        settings.overlap,
        settings.line_threshold,
        device,
    );
    // A symbol with no pattern to turn gets no angle, whatever the two
    // channels said: left in, that noise is read as a difference of angle and
    // splits the region into fragments.
    resolve_angles(&mut grid, symbols);
    read_back_grid_over(grid, lines, picture, ground, symbol_set, into, settings)
}

/// How far apart two densities may be before a picture is resampled from
/// one to the other: a hundredth, which is a pixel in a hundred and less
/// than anything resampling would gain.
pub const RESAMPLE_TOLERANCE: f64 = 0.01;

/// `picture`, drawn at `from` pixels per meter, resampled to `to` — or
/// `None` where the two are close enough that it would change nothing.
///
/// Lanczos either way: a map is thin lines and fine patterns, and a filter
/// which blurs them away going down, or steps them going up, gives the
/// network something it was never shown.
pub fn resample(picture: &RgbImage, from: f64, to: f64) -> Option<RgbImage> {
    if !(from > 0.0 && to > 0.0) || (to / from - 1.0).abs() <= RESAMPLE_TOLERANCE {
        return None;
    }
    let factor = to / from;
    let size = |pixels: u32| ((pixels as f64 * factor).round() as u32).max(1);
    Some(image::imageops::resize(
        picture,
        size(picture.width()),
        size(picture.height()),
        image::imageops::FilterType::Lanczos3,
    ))
}

/// The share of a tile which has to be ink rather than paper for the scale
/// head to be asked about it. A tile of white paper shows nothing of how
/// dense the map is, and the head's answer to it is whatever the training
/// set's average was.
const SCALE_TILE_INK: f64 = 0.05;

/// How bright a pixel has to be, in every channel, to count as paper.
const PAPER: u8 = 240;

/// What a network with a scale head says about how dense a picture is.
pub struct ScaleEstimate {
    /// The median of what the tiles said, as the logarithm of how much denser
    /// than the network's normal density the picture is.
    pub log_scale: f64,
    /// How far the tiles' answers spread: half the distance between the
    /// first and the third quartile of their logarithms. Small where the
    /// picture showed its density plainly, and large where the tiles
    /// disagreed — which is a picture with little to go on.
    pub spread: f64,
    /// How many tiles were asked.
    pub tiles: usize,
}

impl ScaleEstimate {
    /// The density the picture is, in pixels per meter, for a network whose
    /// normal density is `normal`.
    pub fn resolution(&self, normal: f64) -> f64 {
        normal * self.log_scale.exp()
    }
}

/// How dense a network with a scale head says `picture` is, or `None` for a
/// network without one.
///
/// The picture is cut into tiles of `tile` pixels as [`predict_image`] cuts
/// it, at its own density — the head was trained on crops of maps at
/// densities of their own, and is asked the question it was trained on — and
/// every tile with any ink on it is asked. What comes back is the median of
/// the answers, which a few tiles of not much more than paper do not move.
pub fn estimate_scale(
    model: &UNet,
    picture: &RgbImage,
    tile: usize,
    device: &Device,
) -> Option<ScaleEstimate> {
    if !model.has_scale_head() {
        return None;
    }
    let step = 1 << DEPTH;
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let tile = round_up(tile.max(step).min(round_up(width.max(height), step)), step);
    let (padded_width, padded_height) = (width.max(tile), height.max(tile));
    // Side by side rather than overlapping: every tile is asked the same
    // question, and the median wants answers which are not about the same
    // pixels twice.
    let mut inked = Vec::new();
    let mut all = Vec::new();
    for top in starts(padded_height, tile, tile) {
        for left in starts(padded_width, tile, tile) {
            let ink = ink_share(picture, left, top, tile);
            all.push((left, top));
            if ink >= SCALE_TILE_INK {
                inked.push((left, top));
            }
        }
    }
    // A picture which is paper all over still gets an answer, out of all of
    // it, rather than none.
    let asked = if inked.is_empty() { all } else { inked };

    let mut said: Vec<f64> = asked
        .iter()
        .filter_map(|&(left, top)| {
            let input = tile_tensor(picture, left, top, tile, device);
            let scale = model.scale(input)?;
            let values = scale.into_data().try_into_vec::<f32>().ok()?;
            values.first().map(|&value| value as f64)
        })
        .collect();
    if said.is_empty() {
        return None;
    }
    said.sort_by(f64::total_cmp);
    let quantile = |q: f64| {
        let at = q * (said.len() - 1) as f64;
        let (low, high) = (at.floor() as usize, at.ceil() as usize);
        said[low] + (said[high] - said[low]) * (at - low as f64)
    };
    Some(ScaleEstimate {
        log_scale: quantile(0.5),
        spread: (quantile(0.75) - quantile(0.25)) / 2.0,
        tiles: said.len(),
    })
}

/// [`estimate_scale`], as the density the picture is drawn at for a network
/// whose normal density is `normal` pixels per meter.
pub fn estimate_resolution(
    model: &UNet,
    picture: &RgbImage,
    tile: usize,
    normal: f64,
    device: &Device,
) -> Option<f64> {
    estimate_scale(model, picture, tile, device).map(|estimate| estimate.resolution(normal))
}

/// The share of the tile at `(left, top)` which is ink rather than paper,
/// counting what is past the edge of the picture as paper.
fn ink_share(picture: &RgbImage, left: usize, top: usize, tile: usize) -> f64 {
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let mut ink = 0usize;
    for y in top..(top + tile).min(height) {
        for x in left..(left + tile).min(width) {
            let pixel = picture.get_pixel(x as u32, y as u32);
            if pixel.0.iter().any(|&channel| channel < PAPER) {
                ink += 1;
            }
        }
    }
    ink as f64 / (tile * tile) as f64
}

/// What the tree network says about a picture: its cuts and the symbol of
/// each leaf, as plain numbers.
///
/// The picture is reduced to the size the network was trained on, the way the
/// training reduced its pictures; the curves it answers with are in picture
/// coordinates, so they say where to cut a picture of any size.
pub fn tree_answer(model: &TreeNet, picture: &RgbImage, device: &Device) -> (CutTree, TreeAnswer) {
    let size = model.size();
    let small = image::imageops::resize(
        picture,
        size as u32,
        size as u32,
        image::imageops::FilterType::Nearest,
    );
    let mut pixels = vec![0.0f32; 3 * size * size];
    for (x, y, pixel) in small.enumerate_pixels() {
        for channel in 0..3 {
            pixels[channel * size * size + y as usize * size + x as usize] =
                pixel[channel] as f32 / 255.0;
        }
    }
    let input = Tensor::<4>::from_data(TensorData::new(pixels, [1, 3, size, size]), device);
    let answer = model.forward(input);

    let [_, nodes, points, _] = answer.curves.dims();
    let flat: Vec<f32> = answer
        .curves
        .clone()
        .into_data()
        .convert::<f32>()
        .try_to_vec::<f32>()
        .unwrap_or_default();
    let cuts = (0..nodes)
        .map(|node| {
            (0..points)
                .map(|k| {
                    let at = (node * points + k) * 2;
                    (flat[at] as f64, flat[at + 1] as f64)
                })
                .collect()
        })
        .collect();
    let tree = CutTree {
        depth: model.depth(),
        cuts,
        leaf_class: model.leaf_classes(&answer),
    };
    (tree, answer)
}

/// What a tree network says about a whole picture as a grid of symbols: the
/// cuts it makes, made hard and drawn at the picture's own size, and the
/// symbol of each leaf.
///
/// Only for the numbers a grid is good for — how much of the picture is frame,
/// how many symbols it used — and for a picture to look at. The map a tree
/// is read into does not go through it: see [`read_back_tree`]. A tree has no
/// angle to give, so every cell has none.
pub fn predict_tree_image(model: &TreeNet, picture: &RgbImage, device: &Device) -> SymbolGrid {
    let (tree, answer) = tree_answer(model, picture, device);
    tree_grid(
        model,
        &tree,
        answer,
        picture.width() as usize,
        picture.height() as usize,
    )
}

/// [`predict_tree_image`] from an answer already in hand.
fn tree_grid(
    model: &TreeNet,
    tree: &CutTree,
    answer: TreeAnswer,
    width: usize,
    height: usize,
) -> SymbolGrid {
    let leaves = model.leaf_map(answer.curves, width, height);
    let mut grid = SymbolGrid::new(width, height);
    for (class, leaf) in grid.class.iter_mut().zip(leaves) {
        *class = tree
            .leaf_class
            .get(leaf as usize)
            .map_or(BACKGROUND, |&c| c as u16);
    }
    // The frame is the network's last class, and not a symbol of the set.
    grid.frame_is(model.classes() as u16);
    grid
}

/// How many points a cut's Bézier is sampled at when it is written as a
/// shape: finer than the network measures its distances at, which is a
/// matter of cost there and of how smooth a boundary looks here.
pub const TREE_SAMPLES: usize = 64;

/// How far a boundary of a tree's may be moved to be rid of a node, in
/// pixels of the picture: a twentieth of one, which takes the nodes a finely
/// sampled curve does not need and no more. Far below the one pixel
/// [`PREDICTED_TOLERANCE`] allows a staircase, because there is no staircase.
pub const TREE_TOLERANCE: f64 = 0.05;

/// A picture through the tree network, out as a map **without a pixel in
/// between**, and back to a picture to be measured against the one it came
/// from.
///
/// The cuts the network answers with are curves, so the areas they make are
/// worked out from the curves: each piece of the picture is clipped by the
/// cut that divides it, the leaves of one symbol are joined, and the polygons
/// that come out are the map's objects (see [`crate::tree_vector`]). Nothing
/// is rasterized and rounded off again; the boundary of an area is the curve
/// the network drew, sampled at [`TREE_SAMPLES`] points. `settings.simplify`'s
/// tolerance is read in pixels of the picture, as it is for a grid.
///
/// `symbols` is `Catalogue::opaque_areas` of `symbol_set`. The grid in the
/// answer is made for the numbers a grid is good for, and is not what the map
/// was written from.
pub fn read_back_tree(
    model: &TreeNet,
    picture: &RgbImage,
    symbol_set: &Path,
    symbols: &[Entry],
    into: &Path,
    settings: &ReadBackSettings,
    device: &Device,
) -> Result<ReadBack, String> {
    let (tree, answer) = tree_answer(model, picture, device);
    let grid = tree_grid(
        model,
        &tree,
        answer,
        picture.width() as usize,
        picture.height() as usize,
    );
    let ground = ground_of(picture, settings);
    let tolerance = settings.simplify.tolerance / (picture.width().max(picture.height()) as f64);
    let regions = tree.regions(TREE_SAMPLES, model.classes(), tolerance);
    let placement = Placement {
        ground,
        scale_denominator: settings.scale_denominator,
    };
    let objects = objects_of_regions(&regions, symbols, &placement)?;
    let written = write_objects(
        &objects,
        symbol_set,
        into,
        settings.scale_denominator,
        vec![],
    )?;
    finish(grid, written, picture, into, settings, ground)
}

/// A grid of symbols, written as a map and drawn again to be measured against
/// the picture it was read off: the half of [`read_back`] which does not care
/// what network the grid came from.
pub fn read_back_grid(
    grid: SymbolGrid,
    picture: &RgbImage,
    symbol_set: &Path,
    into: &Path,
    settings: &ReadBackSettings,
) -> Result<ReadBack, String> {
    read_back_grid_with_lines(grid, None, picture, symbol_set, into, settings)
}

/// [`read_back_grid`], with the lines of a [`LineGrid`] drawn over the areas.
pub fn read_back_grid_with_lines(
    grid: SymbolGrid,
    lines: Option<LineGrid>,
    picture: &RgbImage,
    symbol_set: &Path,
    into: &Path,
    settings: &ReadBackSettings,
) -> Result<ReadBack, String> {
    let ground = ground_of(picture, settings);
    read_back_grid_over(grid, lines, picture, ground, symbol_set, into, settings)
}

/// [`read_back_grid_with_lines`] with the ground the grid covers given
/// rather than worked out from the picture: the grid may be of a resampled
/// copy of it, and the ground is the original's either way.
fn read_back_grid_over(
    grid: SymbolGrid,
    lines: Option<LineGrid>,
    picture: &RgbImage,
    ground: Rect,
    symbol_set: &Path,
    into: &Path,
    settings: &ReadBackSettings,
) -> Result<ReadBack, String> {
    let written = write_map_with_lines(
        &grid,
        lines.as_ref().map(|found| (found, &settings.lines)),
        symbol_set,
        into,
        &Placement {
            ground,
            scale_denominator: settings.scale_denominator,
        },
        &settings.simplify,
    )?;
    let line_symbols = lines.as_ref().map_or(0, LineGrid::symbols_used);
    let mut done = finish(grid, written, picture, into, settings, ground)?;
    done.line_symbols = line_symbols;
    Ok(done)
}

/// The ground a picture covers, in meters: its own size at the resolution,
/// centred on the origin, which is exactly the square a dataset image covers
/// and generalizes to a picture of any size.
fn ground_of(picture: &RgbImage, settings: &ReadBackSettings) -> Rect {
    let (width, height) = (picture.width() as f64, picture.height() as f64);
    let (ground_width, ground_height) = (width / settings.resolution, height / settings.resolution);
    Rect::from_ltrb(
        -ground_width / 2.0,
        -ground_height / 2.0,
        ground_width / 2.0,
        ground_height / 2.0,
    )
}

/// What every read-back ends in: the map just written is drawn again, over
/// the ground the picture covers, and compared with the picture.
fn finish(
    grid: SymbolGrid,
    written: Written,
    picture: &RgbImage,
    into: &Path,
    settings: &ReadBackSettings,
    ground: Rect,
) -> Result<ReadBack, String> {
    // Drawn from the file just written rather than from the objects which
    // went into it: what is scored is the map somebody else can open, and any
    // disagreement between the two is a bug worth tripping over here.
    let drawing = render_map_over(into, settings.resolution, Extent::Ground(ground))
        .map_err(|e| format!("cannot draw {}: {e}", into.display()))?;
    let drawn = to_rgb_image(&drawing.pixmap);
    let comparison = difference_mask(picture, &drawn, DEFAULT_TOLERANCE, false);

    Ok(ReadBack {
        grid,
        objects: written.objects,
        lines: written.lines,
        line_symbols: 0,
        coords: written.coords,
        drawn,
        comparison,
        warnings: drawing.warnings,
    })
}

/// The choices [`read_back`] does not make for itself.
#[derive(Clone, Debug)]
pub struct ReadBackSettings {
    /// How many pixels square a tile of the picture is.
    pub tile: usize,
    /// How much of each tile is shared with the one before it, in pixels.
    pub overlap: usize,
    /// How many pixels of picture one meter of ground is.
    pub resolution: f64,
    /// The densities the network was trained over, lowest first, in pixels
    /// per meter at `scale_denominator`: a picture outside them is resampled
    /// to the nearer end before it is read, and one inside is read as it is
    /// — see the module documentation. One density twice for a network
    /// trained at one. At another scale than its symbol set's, as many pixels
    /// per meter as make the same number of pixels to a millimeter of paper.
    pub network_resolutions: (f64, f64),
    /// The scale to write the map at.
    pub scale_denominator: i32,
    /// How much of the boundary to keep, and what counts as one object's
    /// angle.
    pub simplify: Simplify,
    /// The probability a pixel has to be given for a line to run through it,
    /// where the network has a lines layer.
    pub line_threshold: f32,
    /// What to keep of the lines found: how long one has to be, and how far
    /// it may be moved to be rid of a node.
    pub lines: LineSettings,
}

impl ReadBackSettings {
    /// The settings a run of `crop` pixels wants, over a picture drawn at
    /// `resolution` with a map of `scale_denominator`: tiles the size the run
    /// trained at, overlapping by [`OVERLAP`] of that, and the two loosened
    /// numbers a grid off a network wants of the vectorizer —
    /// [`PREDICTED_TOLERANCE`] of boundary and [`PREDICTED_SAME_ANGLE`]
    /// between two cells of one object.
    ///
    /// The network is taken to have been trained at `resolution` itself, so
    /// nothing is resampled; [`ReadBackSettings::read_at`] and
    /// [`ReadBackSettings::trained_over`] say otherwise.
    pub fn of(crop: usize, resolution: f64, scale_denominator: i32) -> ReadBackSettings {
        ReadBackSettings {
            tile: crop,
            overlap: (crop as f64 * OVERLAP) as usize,
            resolution,
            network_resolutions: (resolution, resolution),
            scale_denominator,
            simplify: Simplify {
                tolerance: PREDICTED_TOLERANCE,
                same_angle: PREDICTED_SAME_ANGLE,
            },
            line_threshold: DEFAULT_LINE_THRESHOLD,
            lines: LineSettings::default(),
        }
    }

    /// These settings, for a picture drawn at `resolution` pixels per meter
    /// rather than at whatever they said.
    pub fn read_at(mut self, resolution: f64) -> ReadBackSettings {
        self.resolution = resolution;
        self
    }

    /// These settings, for a network trained over densities from `low` to
    /// `high` pixels per meter.
    pub fn trained_over(mut self, low: f64, high: f64) -> ReadBackSettings {
        self.network_resolutions = (low.min(high), low.max(high));
        self
    }

    /// The density the network is shown the picture at: its own, where that
    /// is one the network was trained over, and the nearer end of what it
    /// was trained over where it is not.
    pub fn reading_resolution(&self) -> f64 {
        let (low, high) = self.network_resolutions;
        self.resolution.clamp(low, high)
    }
}

/// The density, in pixels per meter at `scale_denominator`, which puts as
/// many pixels to a millimeter of paper as `resolution` does at
/// `normal_scale`.
///
/// What the network was trained on is pixels to the millimeter of paper —
/// a symbol set fixes its symbols' sizes there — and a map written at
/// another scale covers another amount of ground with the same paper.
pub fn at_scale(resolution: f64, normal_scale: i32, scale_denominator: i32) -> f64 {
    resolution * normal_scale as f64 / scale_denominator as f64
}

/// How far a boundary may be moved to be rid of a node, in cells, when the
/// boundary came from a network.
///
/// One cell, which is one pixel of the picture — a third of a meter of ground
/// at the three pixels per meter a dataset is drawn at. The counterpart of
/// [`PREDICTED_SAME_ANGLE`] for the other of [`Simplify`]'s two numbers, and
/// looser than [`crate::vectorize::Simplify::default`] for the same reason:
/// nought is right for a grid of exact labels, and a grid read off a picture
/// is not one.
///
/// What it buys is measured on the labels themselves, where a boundary is
/// known to be the one a map was drawn with: over a dataset map it takes the
/// 32,261 coordinates of the untouched staircase down to 5,028, and moves
/// three hundredths of a percent of the drawn pixels — half a pixel of edge,
/// which is what a tolerance of one pixel is allowed to be. A node every pixel
/// or two along a boundary which wandered smoothly across the ground is a node
/// the map it came from never had.
///
/// It is not what makes an early epoch's map small: there the objects are
/// single cells in their hundreds of thousands, and a cell has no staircase to
/// simplify. Fewer pictures is the answer to that one — see
/// [`crate::net::image_valid::DEFAULT_IMAGE_VALID`].
pub const PREDICTED_TOLERANCE: f64 = 1.0;

/// How far apart two neighbouring pixels' angles may be and still be one
/// object's, when the angles came from a network.
///
/// Looser than [`crate::vectorize::SAME_ANGLE`], which is meant for the
/// exact angles a label file holds. What a network gives is a continuous
/// field with noise on it, and a threshold of a hundredth of a turn would
/// read that noise as a difference of angle and shatter every turning area
/// into fragments.
pub const PREDICTED_SAME_ANGLE: f32 = 0.05;

/// Turns the angles a network gave into angles a map can be written with:
/// unfolded by each symbol's own symmetry, and taken off entirely where the
/// symbol has no pattern to turn.
///
/// Two things the network cannot do for itself, both of them facts about the
/// symbol set rather than about the picture.
///
/// **Unfolding.** What the network was trained to give is the angle
/// multiplied by however many turns of that symbol look alike — see
/// [`crate::ground_truth::GroundTruth::sin_cos_folded`], and
/// [`crate::symbol_kinds::pattern_symmetry`] for why. So a quarter-symmetric
/// symbol predicted at a whole turn is really at a quarter of one, and
/// dividing is what puts it back. Which of the four quarter turns it was is
/// not in the picture and never was; any of them draws it, and this takes the
/// first.
///
/// **Blanking.** The network gives two numbers for every pixel whatever the
/// symbol is, and on a symbol with nothing to turn they are noise. Left in,
/// that noise is read as a difference of angle and splits the region into
/// fragments — see `vectorize::Simplify::same_angle`.
pub fn resolve_angles(grid: &mut SymbolGrid, symbols: &[Entry]) {
    for (class, rotation) in grid.class.iter().zip(&mut grid.rotation) {
        let symbol = symbols.get(*class as usize);
        if !symbol.is_some_and(|symbol| symbol.turns) || *rotation == NO_ROTATION {
            *rotation = NO_ROTATION;
            continue;
        }
        let order = symbol.map_or(1, |symbol| symbol.symmetry.max(1));
        *rotation = (*rotation / order as f32).rem_euclid(1.0);
    }
}

/// Where the tiles of one row or column start, so that they cover `length`
/// and the last of them ends at it.
fn starts(length: usize, tile: usize, stride: usize) -> Vec<usize> {
    let last = length.saturating_sub(tile);
    let mut starts: Vec<usize> = (0..=last).step_by(stride).collect();
    // The stride rarely divides the picture, and the pixels past the last
    // whole step still want answering: the final tile is slid back to end at
    // the edge rather than hang over it.
    if starts.last() != Some(&last) {
        starts.push(last);
    }
    starts
}

/// `value` rounded up to a multiple of `step`.
fn round_up(value: usize, step: usize) -> usize {
    value.div_ceil(step) * step
}

/// One tile of the picture as the network takes it: `[1, 3, tile, tile]`,
/// planar, scaled to `[-1, 1]`.
///
/// The conversion is the one `crate::net::data` uses to build a training batch,
/// and has to stay the one it uses: a network shown its input on another
/// scale is being shown another picture. Past the edge of the image the tile
/// is white, which is the frame a map is drawn inside.
fn tile_tensor(
    image: &RgbImage,
    left: usize,
    top: usize,
    tile: usize,
    device: &Device,
) -> Tensor<4> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let pixels = tile * tile;
    // White, which is 255 and so 1.0 on the scale below: a picture smaller
    // than a tile is padded with the paper a map leaves around itself.
    let mut planes = vec![1.0f32; 3 * pixels];
    for row in 0..tile {
        let y = top + row;
        if y >= height {
            break;
        }
        for column in 0..tile {
            let x = left + column;
            if x >= width {
                break;
            }
            let pixel = image.get_pixel(x as u32, y as u32);
            for (channel, &value) in pixel.0.iter().enumerate() {
                // Nought to 255 becomes -1 to 1, as `data::MapDataset::get`
                // scales a training crop.
                planes[channel * pixels + row * tile + column] = value as f32 / 127.5 - 1.0;
            }
        }
    }
    Tensor::from_data(TensorData::new(planes, [1, 3, tile, tile]), device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn::config::Config;

    /// A picture at a density the network was trained over is read at its
    /// own; one outside, at the nearer end of the range; and a network
    /// trained at one density reads everything at that one.
    #[test]
    fn a_picture_is_read_at_its_own_density_where_the_network_knows_it() {
        let over = |resolution: f64| {
            ReadBackSettings::of(64, resolution, 4000)
                .trained_over(1.0, 4.0)
                .reading_resolution()
        };
        assert_eq!(over(2.5), 2.5);
        assert_eq!(over(1.0), 1.0);
        assert_eq!(over(8.0), 4.0);
        assert_eq!(over(0.5), 1.0);

        let one = ReadBackSettings::of(64, 1.5, 4000).read_at(5.9);
        assert_eq!(one.reading_resolution(), 1.5);
        assert!(resample(&RgbImage::new(10, 10), 2.5, 2.5).is_none());
        let resampled = resample(&RgbImage::new(100, 60), 8.0, 4.0).expect("halved");
        assert_eq!((resampled.width(), resampled.height()), (50, 30));
    }
    use image::Rgb;
    /// A tree nobody trained, asked about a picture of a shape it was not
    /// built for: the grid is the picture's own size, every cell is a symbol
    /// of the set or the frame, and there is no angle anywhere.
    #[test]
    fn a_tree_reads_a_picture_of_any_size_into_a_grid_of_that_size() {
        use crate::net::tree::TreeNetConfig;

        let device = Device::flex();
        let model = TreeNetConfig::new(3)
            .with_size(32)
            .with_depth(2)
            .init(&device);
        let picture = RgbImage::from_pixel(50, 70, Rgb([200, 30, 30]));
        let grid = predict_tree_image(&model, &picture, &device);
        assert_eq!((grid.width, grid.height), (50, 70));
        assert_eq!(grid.class.len(), 50 * 70);
        assert!(grid.class.iter().all(|&c| c < 3 || c == BACKGROUND));
        assert!(grid.rotation.iter().all(|&r| r == NO_ROTATION));
    }

    /// A network nobody trained, which is enough to ask about shapes: what
    /// comes out of it is arbitrary, and it comes out of it for every pixel.
    fn untrained(classes: usize) -> (UNet, Device) {
        let device = Device::flex();
        let model = UNetConfig::new(classes).with_base_channels(2).init(&device);
        (model, device)
    }

    /// A picture of a few colours, so that not every pixel is the same one.
    fn picture(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| {
            Rgb([(x * 7 % 256) as u8, (y * 11 % 256) as u8, 128])
        })
    }

    #[test]
    fn a_prediction_is_a_grid_of_the_picture() {
        let (model, device) = untrained(3);
        let grid = predict_image(&model, &picture(48, 32), 16, 4, &device);
        assert_eq!((grid.width, grid.height), (48, 32));
        assert_eq!(grid.class.len(), 48 * 32);
        assert_eq!(grid.rotation.len(), 48 * 32);
        // Every cell is a class of the network's or the frame, and nothing
        // else: a class past the list is what `vectorize` refuses.
        for &class in &grid.class {
            assert!(
                class == BACKGROUND || (class as usize) < 3,
                "class {class} is neither a symbol nor the frame"
            );
        }
        // And every angle is a share of a whole turn, or no angle at all.
        for &turn in &grid.rotation {
            assert!(
                turn == NO_ROTATION || (0.0..1.0).contains(&turn),
                "{turn} is not a share of a turn"
            );
        }
    }

    /// The class of a pixel is the argmax of the class logits at that pixel,
    /// and the frame is the one past the symbols.
    ///
    /// Taking a channel out of the head means knowing how the head is laid
    /// out, and getting that wrong is a mistake which still produces a grid
    /// of plausible-looking classes. This is the check on the layout itself,
    /// against burn's own argmax over the same tensor.
    #[test]
    fn the_class_of_a_pixel_is_the_argmax_of_its_logits() {
        let (model, device) = untrained(3);
        let image = picture(16, 16);
        let grid = predict_image(&model, &image, 16, 0, &device);

        let raw = model.forward(tile_tensor(&image, 0, 0, 16, &device));
        // The class logits are the first `classes + 1` channels: one per
        // symbol, and the frame after them.
        let winners = raw
            .slice([0..1, 0..4, 0..16, 0..16])
            .argmax(1)
            .into_data()
            .convert::<i64>()
            .try_into_vec::<i64>()
            .expect("an index per pixel");
        assert_eq!(winners.len(), grid.class.len());
        for (at, &winner) in winners.iter().enumerate() {
            let wanted = if winner == 3 {
                BACKGROUND
            } else {
                winner as u16
            };
            assert_eq!(grid.class[at], wanted, "at pixel {at}");
        }
    }

    /// The angle of a pixel is the sine and cosine channels right after the
    /// class logits, and the lines of a network which has them come after
    /// *those* — they are not the last two channels of the head.
    ///
    /// Taking the angle off the end of the head was right until the head
    /// grew a lines layer, and then it read two line logits as an angle.
    #[test]
    fn the_angle_of_a_pixel_does_not_come_from_the_lines_at_the_end_of_the_head() {
        let device = Device::flex();
        let model = UNetConfig::new(3)
            .with_base_channels(2)
            .with_lines(2)
            .init(&device);
        let image = picture(16, 16);
        let (grid, lines) = predict_image_with_lines(&model, &image, 16, 0, 0.5, &device);
        assert_eq!(lines.expect("a lines layer").classes, 2);

        let raw = model.forward(tile_tensor(&image, 0, 0, 16, &device));
        let values = raw.into_data().try_into_vec::<f32>().expect("floats");
        let pixels = 16 * 16;
        for at in 0..pixels {
            let (sin, cos) = (values[4 * pixels + at], values[5 * pixels + at]);
            if sin.hypot(cos) <= ANGLE_LENGTH {
                assert_eq!(grid.rotation[at], NO_ROTATION, "at pixel {at}");
            } else {
                let turn = (sin.atan2(cos) / std::f32::consts::TAU).rem_euclid(1.0);
                assert!((grid.rotation[at] - turn).abs() < 1e-5, "at pixel {at}");
            }
        }
    }

    /// A line is found where its logit is over the threshold's, in the
    /// channel after the angle's.
    #[test]
    fn a_line_is_found_where_its_probability_is_over_the_threshold() {
        let device = Device::flex();
        let model = UNetConfig::new(3)
            .with_base_channels(2)
            .with_lines(2)
            .init(&device);
        let image = picture(16, 16);
        let raw = model.forward(tile_tensor(&image, 0, 0, 16, &device));
        let values = raw.into_data().try_into_vec::<f32>().expect("floats");
        let pixels = 16 * 16;

        // A network with no lines layer says nothing about lines at all.
        let (_, none) = predict_image_with_lines(&untrained(3).0, &image, 16, 0, 0.5, &device);
        assert!(none.is_none());

        let (_, found) = predict_image_with_lines(&model, &image, 16, 0, 0.5, &device);
        let found = found.expect("a lines layer");
        for line in 0..2 {
            for at in 0..pixels {
                let logit = values[(6 + line) * pixels + at];
                assert_eq!(found.channel(line)[at], (logit > 0.0) as u8);
            }
        }
        // A threshold of nearly one finds almost nothing, and nearly nought
        // almost everything.
        let strict = predict_image_with_lines(&model, &image, 16, 0, 0.999, &device).1;
        let loose = predict_image_with_lines(&model, &image, 16, 0, 0.001, &device).1;
        let count = |grid: &LineGrid| grid.mask.iter().filter(|&&on| on == 1).count();
        assert!(count(&strict.unwrap()) <= count(&found));
        assert!(count(&loose.unwrap()) >= count(&found));
    }

    /// A picture which is one tile across is one tile: the tiling has to
    /// leave that case exactly as a single pass would.
    #[test]
    fn one_tile_of_a_picture_is_the_picture() {
        let (model, device) = untrained(2);
        let image = picture(32, 32);
        let whole = predict_image(&model, &image, 32, 0, &device);
        let tiled = predict_image(&model, &image, 32, 8, &device);
        assert_eq!(whole.class, tiled.class);
    }

    /// A picture smaller than a tile is padded out to something the network
    /// can halve, and cropped back to the size it came in at.
    #[test]
    fn a_picture_smaller_than_a_tile_comes_back_its_own_size() {
        let (model, device) = untrained(2);
        // 20 divides by neither 16 nor the tile it is given.
        let grid = predict_image(&model, &picture(20, 12), 32, 8, &device);
        assert_eq!((grid.width, grid.height), (20, 12));
        assert_eq!(grid.class.len(), 20 * 12);
    }

    /// Every pixel is inside some tile, whatever the tile size and the
    /// stride divide into. A pixel no tile reached would keep the class
    /// `SymbolGrid::new` filled it with and be indistinguishable from the
    /// frame, which is the one way this could go wrong quietly.
    #[test]
    fn the_tiles_of_a_picture_cover_every_pixel_of_it() {
        for (length, tile, stride) in [
            (64, 32, 24),
            (100, 16, 12),
            (1650, 256, 192),
            (20, 32, 32),
            (33, 16, 1),
        ] {
            let mut covered = vec![false; length];
            for start in starts(length, tile, stride) {
                for reached in &mut covered[start..(start + tile).min(length)] {
                    *reached = true;
                }
            }
            assert!(
                covered.iter().all(|&reached| reached),
                "{length} pixels in tiles of {tile} at a stride of {stride} left a gap"
            );
        }
    }

    /// A picture two tiles across comes back whole.
    #[test]
    fn a_tiled_picture_comes_back_whole() {
        let (model, device) = untrained(2);
        let grid = predict_image(&model, &picture(64, 48), 32, 8, &device);
        assert_eq!((grid.width, grid.height), (64, 48));
        assert_eq!(grid.class.len(), 64 * 48);
    }

    #[test]
    fn the_tiles_of_a_row_cover_it_and_end_at_its_edge() {
        // 64 pixels of picture in tiles of 32 at a stride of 24.
        assert_eq!(starts(64, 32, 24), [0, 24, 32]);
        // A stride which divides needs nothing added.
        assert_eq!(starts(64, 32, 16), [0, 16, 32]);
        // One tile is the whole of it.
        assert_eq!(starts(32, 32, 24), [0]);
    }

    /// A symbol with no pattern to turn is given no angle, whatever the two
    /// channels said about it.
    #[test]
    fn a_symbol_with_nothing_to_turn_keeps_no_angle() {
        let symbols: Vec<Entry> = [false, true]
            .into_iter()
            .enumerate()
            .map(|(at, turns)| Entry {
                index: at,
                id: at as i32,
                code: String::new(),
                name: String::new(),
                turns,
                symmetry: 1,
            })
            .collect();

        let mut grid = SymbolGrid::new(3, 1);
        grid.class = vec![0, 1, BACKGROUND];
        grid.rotation = vec![0.25, 0.25, 0.25];
        resolve_angles(&mut grid, &symbols);
        assert_eq!(grid.rotation, [NO_ROTATION, 0.25, NO_ROTATION]);
    }

    /// What a network gives is the angle folded by the symbol's own symmetry
    /// — see [`crate::ground_truth::GroundTruth::sin_cos_folded`] — so this
    /// is where it is divided back down into an angle a map can be written
    /// with.
    #[test]
    fn an_angle_comes_back_out_divided_by_the_symbols_symmetry() {
        let mut symbols = symbols(2);
        symbols[1].turns = true;
        symbols[1].symmetry = 4;

        let mut grid = SymbolGrid::new(4, 1);
        grid.class = vec![1, 1, 1, 1];
        // A whole turn of the folded angle is a quarter turn of the symbol,
        // which is exactly the turn that draws the same picture again.
        grid.rotation = vec![0.0, 0.4, 0.8, 0.99];
        resolve_angles(&mut grid, &symbols);
        for (at, wanted) in [0.0, 0.1, 0.2, 0.2475].into_iter().enumerate() {
            assert!(
                (grid.rotation[at] - wanted).abs() < 1e-6,
                "{at}: {} is not {wanted}",
                grid.rotation[at],
            );
        }
        // Everything it can give lands inside the one turn the picture can
        // tell apart, and none of it past it.
        assert!(grid
            .rotation
            .iter()
            .all(|&turn| (0.0..0.25).contains(&turn)));
    }

    /// A symbol which folds nothing keeps the angle it was given, which is
    /// what every symbol did before there was any folding at all.
    #[test]
    fn a_symmetry_of_one_leaves_the_angle_where_it_was() {
        let mut symbols = symbols(2);
        symbols[1].turns = true;
        symbols[1].symmetry = 1;

        let mut grid = SymbolGrid::new(2, 1);
        grid.class = vec![1, 1];
        grid.rotation = vec![0.3, 0.75];
        resolve_angles(&mut grid, &symbols);
        assert_eq!(grid.rotation, [0.3, 0.75]);
    }

    /// The whole way round: a picture in, a map on disk, a drawing of it and
    /// a comparison against the picture it came from.
    #[test]
    fn read_back_writes_a_map_and_measures_the_drawing_of_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (model, device) = untrained(3);
        let image = picture(48, 32);
        let map = dir.path().join("back.omap");

        let done = read_back(
            &model,
            &image,
            Path::new("tests/data/turning_patterns.xmap"),
            &symbols(3),
            &map,
            &ReadBackSettings::of(16, 2.0, 10000),
            &device,
        )
        .expect("cannot read the picture back");

        assert!(map.is_file(), "the map it wrote is where it said");
        // Drawn over the ground the picture covers, so the two are the same
        // size and the comparison is pixel for pixel.
        assert_eq!((done.drawn.width(), done.drawn.height()), (48, 32));
        assert_eq!(
            (done.comparison.mask.width, done.comparison.mask.height),
            (48, 32)
        );
        // The three shares are shares of the whole picture.
        let (agree, edge, real) = done.shares();
        assert!(
            (agree + edge + real - 1.0).abs() < 1e-9,
            "{agree} + {edge} + {real}"
        );
        assert!((0.0..=1.0).contains(&done.frame_share()));
        assert!(done.symbols_used() <= 3);
    }

    /// Three symbols of a set, the last of which turns.
    fn symbols(count: usize) -> Vec<Entry> {
        (0..count)
            .map(|at| Entry {
                index: at,
                id: at as i32,
                code: String::new(),
                name: String::new(),
                turns: at + 1 == count,
                symmetry: 1,
            })
            .collect()
    }

    /// A model saved and loaded back says the same thing about a picture.
    ///
    /// A file which kept weights at half precision would be lossy, and the
    /// running statistics of a batch norm are the numbers that loses most —
    /// so this is the check that what a run folder holds is still the network
    /// which trained.
    #[test]
    fn a_model_saved_and_read_back_says_what_it_said() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (model, device) = untrained(3);
        let image = picture(48, 32);
        let before = predict_image(&model, &image, 16, 4, &device);

        TrainingConfig::new(3)
            .with_base_channels(2)
            .save(dir.path().join("training.json"))
            .expect("cannot write the configuration");
        save_weights(model.clone(), &dir.path().join(BEST_WEIGHTS))
            .expect("cannot write the weights");

        let (read, _) = load(dir.path(), None, &device).expect("cannot read them back");
        let after = predict_image(&read, &image, 16, 4, &device);
        assert_eq!(before.class, after.class);
    }

    /// Reading a grid off a network loosens both of the vectorizer's numbers,
    /// which is the whole difference between these settings and the ones a
    /// file of exact labels is vectorized with.
    ///
    /// Stated as a comparison rather than as the two values, since what
    /// matters is that neither is the default: a boundary a network drew is
    /// not the staircase it comes out as, and its angles are a noisy field
    /// rather than one angle per object.
    #[test]
    fn a_grid_off_a_network_is_vectorized_more_loosely_than_a_file_of_labels() {
        let exact = Simplify::default();
        let predicted = ReadBackSettings::of(256, 3.0, 10000).simplify;
        assert!(
            predicted.tolerance > exact.tolerance,
            "{} is not looser than {}",
            predicted.tolerance,
            exact.tolerance,
        );
        assert!(
            predicted.same_angle > exact.same_angle,
            "{} is not looser than {}",
            predicted.same_angle,
            exact.same_angle,
        );
        // And a tolerance is in cells, so one which reached across a crop
        // would take a region's boundary off altogether.
        assert!(predicted.tolerance < 16.0);
    }

    /// A run folder with no weights in it says so, rather than handing back a
    /// network of whatever `init` happened to draw.
    #[test]
    fn a_run_without_weights_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let device = Device::flex();

        let error = load(dir.path(), None, &device).expect_err("no configuration to read");
        assert!(error.contains("training.json"), "{error}");

        TrainingConfig::new(3)
            .save(dir.path().join("training.json"))
            .expect("cannot write the configuration");
        let error = load(dir.path(), None, &device).expect_err("no weights to read");
        assert!(error.contains("weights"), "{error}");
    }
}
