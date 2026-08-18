//! Running a trained network over a whole picture.
//!
//! [`crate::training`] leaves a folder behind; this is what reads one back
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
//! The recorder has to be the one which saved: `CompactRecorder`, which is
//! burn's named-messagepack file recorder at half precision. `best.mpk` is
//! the epoch which validated best; a `checkpoint/007/model.mpk` is the same
//! format and loads the same way, which is what `weights` is for.
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
//! A [`SymbolGrid`], which is what [`maur_o::vectorize`] turns into a map.
//! The class of a pixel is the argmax over the `classes + 1` logits of
//! [`UNet::forward`], with a win for the frame becoming
//! [`BACKGROUND`](maur_o::ground_truth::BACKGROUND); the angle is
//! `atan2(sin, cos)` of the last two channels, and
//! [`NO_ROTATION`](maur_o::ground_truth::NO_ROTATION) where the pair is
//! shorter than half a unit — the same threshold [`crate::training`] masks
//! its rotation score with, and for the same reason: a label is either the
//! zero vector or a point on the unit circle, so anything in between is the
//! network saying it does not know.
//!
//! What it does **not** do is decide that a symbol with no pattern to turn
//! has no angle. That needs the symbol set, which is a thing about the map
//! rather than about the network — see [`blank_still_angles`].
//!
//! # The whole way round
//!
//! [`read_back`] is all of it in one call: a picture in, the map the network
//! read out of it written to a file, that map drawn again, and the drawing
//! measured against the picture it came from. The `image_to_map` tool is that
//! function with a command line around it, and [`crate::image_valid`] is that
//! function once per epoch of a run — which is the point of it being one
//! function. A sheet a run leaves behind has to show what the tool would
//! give, or it is a picture of something else.

use std::path::Path;

use burn::config::Config;
use burn::module::Module;
use burn::prelude::Backend;
use burn::record::CompactRecorder;
use burn::tensor::{Tensor, TensorData};
use image::RgbImage;

use maur_o::differences::{difference_mask, Comparison, DEFAULT_TOLERANCE};
use maur_o::geometry::Rect;
use maur_o::ground_truth::{BACKGROUND, NO_ROTATION};
use maur_o::render::{render_map_over, to_rgb_image, Extent};
use maur_o::symbol_kinds::Entry;
use maur_o::vectorize::{write_map, Placement, Simplify, SymbolGrid};

use crate::training::TrainingConfig;
use crate::unet::{UNet, UNetConfig, DEPTH};

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
/// `weights` names the record to load, and is `<run>/best.mpk` where it is
/// `None`. A checkpoint — `<run>/checkpoint/007/model.mpk` — is the same
/// format and may be named instead, which is how a run is looked at as it was
/// part way through.
pub fn load<B: Backend>(
    run: &Path,
    weights: Option<&Path>,
    device: &B::Device,
) -> Result<(UNet<B>, TrainingConfig), String> {
    let configuration = run.join("training.json");
    // The shape of the module has to be known before a record can be loaded
    // into it, and a record does not carry it.
    let config = TrainingConfig::load(&configuration).map_err(|e| {
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
        .init::<B>(device)
        .load_file(weights, &CompactRecorder::new(), device)
        .map_err(|e| {
            format!(
                "cannot read the weights from {}: {e}\nA run which was stopped before it \
                 finished an epoch has none: there is no best.mpk and checkpoint/ is empty.",
                weights.display(),
            )
        })?;
    Ok((model, config))
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
pub fn predict_image<B: Backend>(
    model: &UNet<B>,
    image: &RgbImage,
    tile: usize,
    overlap: usize,
    device: &B::Device,
) -> SymbolGrid {
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
    // How far from its tile's edge each pixel's answer was found, and one
    // more than that so nought can mean no answer yet. A later tile only
    // overwrites a pixel it saw better than the tile before it did.
    let mut margin = vec![0u32; width * height];

    for top in starts(padded_height, tile, stride) {
        for left in starts(padded_width, tile, stride) {
            let input = tile_tensor(image, left, top, tile, device);
            // `classes + 1` class logits, the frame last, then the sine and
            // the cosine of the angle.
            let raw = model.forward(input);
            let [_, channels, _, _] = raw.dims();
            let values = raw
                .into_data()
                .into_vec::<f32>()
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

                    let (sin, cos) = (of(channels - 2), of(channels - 1));
                    grid.rotation[at] = if sin.hypot(cos) <= ANGLE_LENGTH {
                        // Not an angle it got wrong: an angle it declined to
                        // give, which is what the zero vector says.
                        NO_ROTATION
                    } else {
                        let turn = sin.atan2(cos) / std::f32::consts::TAU;
                        turn.rem_euclid(1.0)
                    };
                }
            }
        }
    }
    grid
}

/// What one picture came to, on the way through the network and back out as
/// a map.
pub struct ReadBack {
    /// The grid the network said, with the still symbols' angles taken off.
    pub grid: SymbolGrid,
    /// How many path objects the map came to.
    pub objects: usize,
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
/// `symbols` its opaque areas, which is [`Catalogue::opaque_areas`] of the
/// same file.
#[allow(clippy::too_many_arguments)]
pub fn read_back<B: Backend>(
    model: &UNet<B>,
    picture: &RgbImage,
    symbol_set: &Path,
    symbols: &[Entry],
    into: &Path,
    settings: &ReadBackSettings,
    device: &B::Device,
) -> Result<ReadBack, String> {
    let mut grid = predict_image(model, picture, settings.tile, settings.overlap, device);
    // A symbol with no pattern to turn gets no angle, whatever the two
    // channels said: left in, that noise is read as a difference of angle and
    // splits the region into fragments.
    blank_still_angles(&mut grid, symbols);

    let (width, height) = (picture.width() as f64, picture.height() as f64);
    let (ground_width, ground_height) = (width / settings.resolution, height / settings.resolution);
    let ground = Rect::from_ltrb(
        -ground_width / 2.0,
        -ground_height / 2.0,
        ground_width / 2.0,
        ground_height / 2.0,
    );
    let written = write_map(
        &grid,
        symbol_set,
        into,
        &Placement {
            ground,
            scale_denominator: settings.scale_denominator,
        },
        &settings.simplify,
    )?;

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
    /// The scale to write the map at.
    pub scale_denominator: i32,
    /// How much of the boundary to keep, and what counts as one object's
    /// angle.
    pub simplify: Simplify,
}

impl ReadBackSettings {
    /// The settings a run of `crop` pixels wants, over a picture drawn at
    /// `resolution` with a map of `scale_denominator`: tiles the size the run
    /// trained at, overlapping by [`OVERLAP`] of that, every node the
    /// staircase asks for, and [`PREDICTED_SAME_ANGLE`] between two cells of
    /// one object.
    pub fn of(crop: usize, resolution: f64, scale_denominator: i32) -> ReadBackSettings {
        ReadBackSettings {
            tile: crop,
            overlap: (crop as f64 * OVERLAP) as usize,
            resolution,
            scale_denominator,
            simplify: Simplify {
                tolerance: 0.0,
                same_angle: PREDICTED_SAME_ANGLE,
            },
        }
    }
}

/// How far apart two neighbouring pixels' angles may be and still be one
/// object's, when the angles came from a network.
///
/// Looser than [`maur_o::vectorize::SAME_ANGLE`], which is meant for the
/// exact angles a label file holds. What a network gives is a continuous
/// field with noise on it, and a threshold of a hundredth of a turn would
/// read that noise as a difference of angle and shatter every turning area
/// into fragments.
pub const PREDICTED_SAME_ANGLE: f32 = 0.05;

/// Takes the angle off every cell whose symbol has no pattern to turn.
///
/// The network gives two numbers for every pixel whatever the symbol is, and
/// on a symbol with nothing to turn they are noise. Left in, that noise is
/// read as a difference of angle and splits the region into fragments — see
/// `vectorize::Simplify::same_angle` — so it goes here, where the symbol set
/// is known and the network is not.
pub fn blank_still_angles(grid: &mut SymbolGrid, symbols: &[Entry]) {
    for (class, rotation) in grid.class.iter().zip(&mut grid.rotation) {
        let turns = symbols
            .get(*class as usize)
            .is_some_and(|symbol| symbol.turns);
        if !turns {
            *rotation = NO_ROTATION;
        }
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
/// The conversion is the one `crate::data` uses to build a training batch,
/// and has to stay the one it uses: a network shown its input on another
/// scale is being shown another picture. Past the edge of the image the tile
/// is white, which is the frame a map is drawn inside.
fn tile_tensor<B: Backend>(
    image: &RgbImage,
    left: usize,
    top: usize,
    tile: usize,
    device: &B::Device,
) -> Tensor<B, 4> {
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
    use burn::backend::ndarray::{NdArray, NdArrayDevice};
    use image::Rgb;

    type TestBackend = NdArray<f32>;

    /// A network nobody trained, which is enough to ask about shapes: what
    /// comes out of it is arbitrary, and it comes out of it for every pixel.
    fn untrained(classes: usize) -> (UNet<TestBackend>, NdArrayDevice) {
        let device = NdArrayDevice::default();
        let model = UNetConfig::new(classes)
            .with_base_channels(2)
            .init::<TestBackend>(&device);
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

        let raw = model.forward(tile_tensor::<TestBackend>(&image, 0, 0, 16, &device));
        // The class logits are the first `classes + 1` channels: one per
        // symbol, and the frame after them.
        let winners = raw
            .slice([0..1, 0..4, 0..16, 0..16])
            .argmax(1)
            .into_data()
            .convert::<i64>()
            .into_vec::<i64>()
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
            })
            .collect();

        let mut grid = SymbolGrid::new(3, 1);
        grid.class = vec![0, 1, BACKGROUND];
        grid.rotation = vec![0.25, 0.25, 0.25];
        blank_still_angles(&mut grid, &symbols);
        assert_eq!(grid.rotation, [NO_ROTATION, 0.25, NO_ROTATION]);
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
            Path::new("../tests/data/turning_patterns.xmap"),
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
            })
            .collect()
    }

    /// A model saved and loaded back says the same thing about a picture.
    ///
    /// The recorder keeps weights at half precision, which is lossy, and the
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
        model
            .save_file(dir.path().join(BEST_WEIGHTS), &CompactRecorder::new())
            .expect("cannot write the weights");

        let (read, _) =
            load::<TestBackend>(dir.path(), None, &device).expect("cannot read them back");
        let after = predict_image(&read, &image, 16, 4, &device);
        assert_eq!(before.class, after.class);
    }

    /// A run folder with no weights in it says so, rather than handing back a
    /// network of whatever `init` happened to draw.
    #[test]
    fn a_run_without_weights_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let device = NdArrayDevice::default();

        let error = load::<TestBackend>(dir.path(), None, &device)
            .err()
            .expect("no configuration to read");
        assert!(error.contains("training.json"), "{error}");

        TrainingConfig::new(3)
            .save(dir.path().join("training.json"))
            .expect("cannot write the configuration");
        let error = load::<TestBackend>(dir.path(), None, &device)
            .err()
            .expect("no weights to read");
        assert!(error.contains("weights"), "{error}");
    }
}
