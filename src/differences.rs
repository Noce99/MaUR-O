//! Shows where the benchmark renderings differ from their reference images.
//!
//! It compares an `expected/` folder of reference images against a
//! `predictions/` folder of this project's own renderings, image by image.
//! Identical pairs are counted and skipped, and for every differing pair a
//! subfolder is written, holding
//!
//! ```text
//! side_by_side.png    the whole of both images, expected left, predicted right
//! diff.png            black where the two agree, red where they really differ,
//!                     dim orange where antialiasing explains it
//! crop_1_XxY.png      the worst region, expected, predicted and diff, enlarged
//! crop_2_XxY.png      the second worst region, and so on
//! ```
//!
//! `side_by_side.png` is missing where the map is too large for it to be
//! worth decoding a second time just to shrink, and the pair is listed in
//! [`Report::no_overview`] instead. The crops, which is what a difference is
//! actually diagnosed from, are written all the same.
//!
//! The crop file names carry the top left corner of the region in the image,
//! so a region can be found again in the full size images.
//!
//! The central problem this module exists to solve is that a raw pixel diff
//! of two renderings is nearly useless. Almost every pixel it flags is an
//! edge both renderers drew and merely disagreed about the coverage of by a
//! shade — unavoidable between two different rasterizers, present in the
//! thousands on any real map, and enough to bury the handful of pixels that
//! mean something. A suite whose every map is "2% different" cannot be read,
//! and cannot show a regression.
//!
//! So every differing pixel is classified by [`is_antialiasing`] into a
//! difference two rasterizers can legitimately have about a shared edge, and
//! one they cannot. Both are counted and both are drawn — orange and red in
//! `diff.png` — but only the second kind decides which regions are worth
//! cropping out, and a pair with none of them gets no folder at all. What is
//! left in `differences/` after a run is therefore the list of things worth
//! looking at, which is the whole point. Set `keep_antialiasing` to turn the
//! classification off and have every differing pixel counted as real.
//!
//! Where a choice had no principled answer, this follows the convention the
//! usual Python imaging stack uses — Pillow's rounding, its text anchoring,
//! numpy's tie-break on an argmax — so that a number here means what the same
//! number computed with those tools would mean.

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

/// The colour of the labels and of the background they sit on. The background
/// also fills the canvas where one image is smaller than the other, where it is
/// dark enough to be told apart from anything the maps are drawn with.
const LABEL_BACKGROUND: Rgb<u8> = Rgb([32, 32, 32]);
const LABEL_FOREGROUND: Rgb<u8> = Rgb([255, 255, 255]);

/// The colour a differing pixel gets in diff.png and in the crops.
const DIFFERENCE_COLOUR: Rgb<u8> = Rgb([255, 0, 0]);

/// The colour a pixel gets when its difference is put down to antialiasing:
/// dimmer than the real ones, so that a crop full of it reads at a glance as
/// a crop with nothing in it.
const ANTIALIASING_COLOUR: Rgb<u8> = Rgb([160, 80, 0]);

/// The side of a block the difference mask is scored in, when looking for
/// the regions worth cropping out.
const BLOCK: u32 = 16;

/// The gap between the panels of a composed image.
const GAP: u32 = 8;

/// Above this many decoded bytes, [`write_side_by_side`] is skipped rather
/// than run: it reads a whole image a second time only to shrink it into an
/// overview, and for a big enough map that second decode is real memory and
/// real time spent on a preview the crops already make unnecessary. 512MiB
/// is the `image` crate's own default decode limit — the point past which it
/// considers a single image large — reused here for the same reasoning
/// applied to a redundant decode rather than the mandatory first one.
const MAX_OVERVIEW_BYTES: u64 = 512 * 1024 * 1024;

/// How a comparison run is to be carried out and reported.
#[derive(Debug, Clone)]
pub struct Options {
    /// Per pixel difference, summed over red, green and blue, which does not
    /// count as a difference.
    pub tolerance: i32,
    /// How many regions to crop out, 0 for as many as it takes to cover
    /// every difference.
    pub crops: usize,
    /// The side of a cropped region, in pixels.
    pub crop_size: u32,
    /// The width a cropped region is enlarged to.
    pub zoom: u32,
    /// The width the whole images are scaled down to in side_by_side.png, 0
    /// for their full size.
    pub overview: u32,
    /// Only compare images whose name contains this.
    pub filter: String,
    /// Count differences antialiasing explains as real ones, i.e. turn the
    /// classification off and report every differing pixel.
    pub keep_antialiasing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tolerance: DEFAULT_TOLERANCE,
            crops: 0,
            crop_size: 128,
            zoom: 512,
            overview: 2000,
            filter: String::new(),
            keep_antialiasing: false,
        }
    }
}

/// The error a pixel is allowed before it counts as wrong.
///
/// One per channel: a flat area of one colour comes out of two renderers a
/// unit apart per channel often enough, from nothing more than where each
/// one's float-to-integer conversion of the same colour lands. Summed over
/// red, green and blue that is 3, and forgiving it stops a whole area being
/// reported over a rounding difference nobody can see.
pub const DEFAULT_TOLERANCE: i32 = 3;

/// What a run of [`compare`] found.
#[derive(Debug, Default)]
pub struct Report {
    /// How many pairs were compared.
    pub total: usize,
    /// How many came out pixel for pixel identical.
    pub identical: usize,
    /// Pairs with at least one real difference. Only these get a folder.
    pub differing: usize,
    /// Pairs which differ, but only in ways antialiasing explains.
    pub antialiasing_only: usize,
    /// Reference images with no rendering next to them, by name.
    pub missing: Vec<String>,
    /// Differing pairs too large to be worth an overview, by name: their
    /// `differences/` folder holds the crops and no `side_by_side.png`.
    pub no_overview: Vec<String>,
    /// One row per pair actually compared, in the order the suite runs.
    /// [`write_results`] sorts a copy for the table; this stays as it ran.
    pub rows: Vec<Row>,
}

/// How one pair of images compared, as the results table reports it.
#[derive(Debug, Clone)]
pub struct Row {
    /// The map's name, without a folder or a suffix.
    pub name: String,
    /// The fraction of the union of the two images which differs at all.
    pub differing: f64,
    /// The fraction which differs in a way antialiasing does not explain.
    pub real: f64,
    /// The largest per-pixel error, summed over red, green and blue, so from
    /// 0 to 765.
    pub largest: i32,
    /// The mean error and its standard deviation over the wrong pixels
    /// alone, or `None` where no pixel was wrong.
    pub error: Option<(f64, f64)>,
}

/// Everything measuring a pair of images yields.
pub struct Comparison {
    /// Which pixels differ, and what each difference was put down to.
    pub mask: Mask,
    /// The largest per-pixel error, summed over red, green and blue.
    pub largest: i32,
    /// The mean of that error and its population standard deviation, taken
    /// over the wrong pixels only — the ones whose error is above the
    /// tolerance.
    ///
    /// Averaging over every pixel instead would say almost nothing: a map is
    /// mostly white paper which both renderers agree on, so the mean would
    /// measure how much blank space the map has rather than how wrong the
    /// rendering is. `None` where no pixel was wrong, since there is then
    /// nothing to average.
    ///
    /// Pixels outside the two images' overlap count as wrong in the mask but
    /// have no error to measure, so they are not in this average either.
    pub error: Option<(f64, f64)>,
}

/// The two images agree at this pixel, within the tolerance.
pub const AGREE: u8 = 0;
/// They disagree, but only in a way two rasterizers can disagree about an
/// edge they both drew. See [`is_antialiasing`].
pub const ANTIALIASING: u8 = 1;
/// They disagree in a way antialiasing does not explain.
pub const REAL: u8 = 2;

/// Which pixels of a pair of images differ, and what their difference was
/// put down to.
///
/// The mask covers the union of the two images: where they do not overlap,
/// every pixel counts as differing, and as a real difference rather than an
/// antialiasing one — a rendering of the wrong size is not an edge effect.
pub struct Mask {
    /// The width of the union of the two images, in pixels.
    pub width: u32,
    /// Its height.
    pub height: u32,
    /// One byte per pixel, row by row: [`AGREE`], [`ANTIALIASING`] or [`REAL`].
    bits: Vec<u8>,
}

impl Mask {
    fn class(&self, x: u32, y: u32) -> u8 {
        self.bits[(y as usize) * (self.width as usize) + x as usize]
    }

    fn any(&self) -> bool {
        self.bits.iter().any(|&b| b != AGREE)
    }

    /// How many pixels differ at all, however the difference is explained.
    fn count(&self) -> u64 {
        self.bits.iter().filter(|&&b| b != AGREE).count() as u64
    }

    /// How many differ in a way antialiasing does not explain.
    pub fn count_real(&self) -> u64 {
        self.bits.iter().filter(|&&b| b == REAL).count() as u64
    }

    fn size(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// How many real differences are inside a square region.
    ///
    /// Antialiasing is deliberately not counted: the regions to crop out are
    /// chosen by this, and a crop of an edge both renderers drew is a crop
    /// nobody needs to open.
    fn count_in(&self, x: u32, y: u32, size: u32) -> u64 {
        let mut total = 0;
        for row in y..(y + size).min(self.height) {
            let start = (row as usize) * (self.width as usize);
            for column in x..(x + size).min(self.width) {
                total += u64::from(self.bits[start + column as usize] == REAL);
            }
        }
        total
    }
}

/// Banker's rounding: a tie goes to the even number rather than away from
/// zero, the way Python's `round` and numpy both do it. The image sizes below
/// are computed with it, and a size one pixel off is a report whose panels do
/// not line up.
fn python_round(value: f64) -> f64 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 && rounded % 2.0 != 0.0 {
        rounded - value.signum()
    } else {
        rounded
    }
}

/// Whether `image` has a colour step somewhere in the 3x3 window around
/// (x, y) — that is, whether it drew an edge there.
///
/// Measured as the range the window's colours span, summed over red, green
/// and blue, against the same tolerance a pixel is judged by, so that a flat
/// area which merely rounds differently across it is not an edge.
///
/// The window is clipped to the region the two images share, so nothing is
/// ever explained by a pixel only one of them has.
fn has_edge(image: &RgbImage, x: u32, y: u32, width: u32, height: u32, tolerance: i32) -> bool {
    let mut low = [255u8; 3];
    let mut high = [0u8; 3];
    for row in y.saturating_sub(1)..=(y + 1).min(height - 1) {
        for column in x.saturating_sub(1)..=(x + 1).min(width - 1) {
            let pixel = image.get_pixel(column, row);
            for channel in 0..3 {
                low[channel] = low[channel].min(pixel[channel]);
                high[channel] = high[channel].max(pixel[channel]);
            }
        }
    }
    (0..3).map(|channel| high[channel] as i32 - low[channel] as i32).sum::<i32>() > tolerance
}

/// Whether the disagreement at (x, y) is one two rasterizers can have about
/// an edge they both drew.
///
/// Antialiasing happens only at an edge, so a pixel qualifies only where
/// *both* renderings have one. Requiring it of both is what carries the
/// weight: where only one of them has an edge, one of them drew something
/// the other did not, and that is never an edge effect. A missing symbol, an
/// extra mark, a shape well out of place and a rendering of the wrong size
/// all fail on that.
///
/// An earlier version of this asked something stricter — that each image's
/// colour lie inside the range the other takes in the window, on the grounds
/// that a coverage disagreement can only remix colours both images have
/// there. It reads well and it does not work, because it needs the colours
/// being mixed to be visible somewhere nearby. A map is full of features
/// thinner than a pixel: at a road's casing the pixels are 59% asphalt, 24%
/// white and 17% black, from a gap of white paper narrower than one pixel,
/// so pure white appears nowhere in the neighbourhood at any window size and
/// the blend sits outside the range of everything on either side of it. That
/// left a quarter of the suite reporting road casings as real differences.
///
/// What this cannot do, and no local test can, is tell antialiasing from a
/// shape drawn under a pixel out of place: a line one pixel to the left has
/// exactly the signature of a coverage disagreement. The 3x3 window is
/// therefore also the statement of how much positional disagreement is
/// forgiven — one pixel, which is how far either rasterizer spreads an edge.
/// By the same token a colour error confined to the pixels of an edge is
/// forgiven; over any area more than a pixel wide the interior has no edge
/// in either image, so it is still reported.
pub fn is_antialiasing(
    expected: &RgbImage,
    predicted: &RgbImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    tolerance: i32,
) -> bool {
    has_edge(expected, x, y, width, height, tolerance)
        && has_edge(predicted, x, y, width, height, tolerance)
}

/// Which pixels differ, by how much at the worst pixel, and by how much on
/// average.
///
/// The difference of a pixel is the sum of the absolute per-channel
/// differences over red, green and blue, so it runs from 0 to 765 and a
/// tolerance is read on that scale. Summing rather than taking the largest
/// channel is what makes a colour that is slightly off in all three count as
/// more wrong than one that is slightly off in a single channel.
pub fn difference_mask(
    expected: &RgbImage,
    predicted: &RgbImage,
    tolerance: i32,
    keep_antialiasing: bool,
) -> Comparison {
    let width = expected.width().max(predicted.width());
    let height = expected.height().max(predicted.height());
    let shared_width = expected.width().min(predicted.width());
    let shared_height = expected.height().min(predicted.height());

    // Everything starts real; only the shared region is looked at below, and
    // what is left over is the size mismatch, which antialiasing cannot
    // account for.
    let mut bits = vec![REAL; (width as usize) * (height as usize)];
    let mut largest = 0;
    // Summed over the wrong pixels only. The error is at most 765 over at
    // most a few hundred megapixels, so neither sum can come close to
    // overflowing.
    let (mut wrong, mut sum, mut sum_of_squares) = (0u64, 0u64, 0u64);
    for y in 0..shared_height {
        let row = (y as usize) * (width as usize);
        for x in 0..shared_width {
            let a = expected.get_pixel(x, y);
            let b = predicted.get_pixel(x, y);
            let distance = (a[0] as i32 - b[0] as i32).abs()
                + (a[1] as i32 - b[1] as i32).abs()
                + (a[2] as i32 - b[2] as i32).abs();
            largest = largest.max(distance);
            if distance > tolerance {
                let antialiasing = !keep_antialiasing
                    && is_antialiasing(
                        expected,
                        predicted,
                        x,
                        y,
                        shared_width,
                        shared_height,
                        tolerance,
                    );
                bits[row + x as usize] = if antialiasing { ANTIALIASING } else { REAL };
                wrong += 1;
                sum += distance as u64;
                sum_of_squares += (distance as u64) * (distance as u64);
            } else {
                bits[row + x as usize] = AGREE;
            }
        }
    }

    let error = (wrong > 0).then(|| {
        let mean = sum as f64 / wrong as f64;
        // The population variance, which cannot go negative except by
        // rounding; clamped so that the square root stays real.
        let variance = (sum_of_squares as f64 / wrong as f64 - mean * mean).max(0.0);
        (mean, variance.sqrt())
    });

    Comparison { mask: Mask { width, height, bits }, largest, error }
}

/// The most differing regions first, as (x, y, differing pixels) tuples.
///
/// The image is scored in small blocks, and the highest scoring block which
/// is not yet covered by a region becomes the centre of the next one.
/// Regions therefore do not overlap, and a single dense cluster of
/// differences does not take all of them.
///
/// Without a count every differing pixel ends up inside a region, which is
/// the useful default: a difference which is not cropped out is a difference
/// which has not been looked at.
pub fn worst_regions(mask: &Mask, count: usize, size: u32) -> Vec<(u32, u32, u64)> {
    let size = size.min(mask.height).min(mask.width);

    let blocks_y = mask.height.div_ceil(BLOCK);
    let blocks_x = mask.width.div_ceil(BLOCK);
    let mut scores = vec![0i64; (blocks_y as usize) * (blocks_x as usize)];
    for y in 0..mask.height {
        let block_row = (y / BLOCK) as usize * blocks_x as usize;
        for x in 0..mask.width {
            // Real differences only: a region is worth cropping out for what
            // is wrong in it, not for the edges both renderers drew.
            if mask.class(x, y) == REAL {
                scores[block_row + (x / BLOCK) as usize] += 1;
            }
        }
    }

    let mut regions = Vec::new();
    while count == 0 || regions.len() < count {
        // The first of the highest scoring blocks, in row-major order, the
        // same one numpy's argmax picks.
        let mut index = 0;
        for (i, &score) in scores.iter().enumerate() {
            if score > scores[index] {
                index = i;
            }
        }
        if scores[index] <= 0 {
            break;
        }
        let block_y = index as u32 / blocks_x;
        let block_x = index as u32 % blocks_x;

        let centred = |block: u32, limit: u32| -> u32 {
            let value = block as i64 * BLOCK as i64 + (BLOCK / 2) as i64 - (size / 2) as i64;
            value.clamp(0, (limit - size) as i64) as u32
        };
        let x = centred(block_x, mask.width);
        let y = centred(block_y, mask.height);
        regions.push((x, y, mask.count_in(x, y, size)));

        // Do not pick anything inside the region just taken again.
        for block in (y / BLOCK)..(y + size).div_ceil(BLOCK).min(blocks_y) {
            let row = block as usize * blocks_x as usize;
            for column in (x / BLOCK)..(x + size).div_ceil(BLOCK).min(blocks_x) {
                scores[row + column as usize] = 0;
            }
        }
    }
    regions
}

/// A new canvas of the given size, filled with the label background.
fn canvas(width: u32, height: u32) -> RgbImage {
    RgbImage::from_pixel(width.max(1), height.max(1), LABEL_BACKGROUND)
}

/// Copies `source` onto `target` with its top left corner at (x, y).
fn paste(target: &mut RgbImage, source: &RgbImage, x: u32, y: u32) {
    for row in 0..source.height().min(target.height().saturating_sub(y)) {
        for column in 0..source.width().min(target.width().saturating_sub(x)) {
            target.put_pixel(x + column, y + row, *source.get_pixel(column, row));
        }
    }
}

/// The region at the given position, padded where the image ends before it.
fn crop(image: &RgbImage, x: u32, y: u32, size: u32) -> RgbImage {
    let mut result = canvas(size, size);
    for row in 0..size.min(image.height().saturating_sub(y)) {
        for column in 0..size.min(image.width().saturating_sub(x)) {
            result.put_pixel(column, row, *image.get_pixel(x + column, y + row));
        }
    }
    result
}

/// The same region of the difference mask: black where the two agree, red
/// where they really differ, dim orange where antialiasing explains it.
fn crop_of_mask(mask: &Mask, x: u32, y: u32, size: u32) -> RgbImage {
    let mut result = RgbImage::from_pixel(size.max(1), size.max(1), Rgb([0, 0, 0]));
    for row in 0..size.min(mask.height.saturating_sub(y)) {
        for column in 0..size.min(mask.width.saturating_sub(x)) {
            match mask.class(x + column, y + row) {
                REAL => result.put_pixel(column, row, DIFFERENCE_COLOUR),
                ANTIALIASING => result.put_pixel(column, row, ANTIALIASING_COLOUR),
                _ => {}
            }
        }
    }
    result
}

/// Enlarges an image by a whole number factor, without smoothing.
///
/// Nearest neighbour: these differences are a pixel wide, and smoothing them
/// away is exactly what must not happen here.
fn magnify(image: &RgbImage, factor: u32) -> RgbImage {
    let mut result = RgbImage::new(image.width() * factor, image.height() * factor);
    for y in 0..result.height() {
        for x in 0..result.width() {
            result.put_pixel(x, y, *image.get_pixel(x / factor, y / factor));
        }
    }
    result
}

/// Puts labelled images next to each other on a single canvas.
///
/// The panels keep their size and are aligned at the top, so that images of
/// different size stay comparable.
fn compose(panels: &[RgbImage], labels: &[String]) -> RgbImage {
    let widest = panels.iter().map(|p| p.width()).max().unwrap_or(1);
    let font_size = (widest / 40).clamp(12, 40);
    let bar = font_size + 8;

    let width: u32 = panels.iter().map(|p| p.width()).sum::<u32>() + GAP * (panels.len() as u32 - 1);
    let height = panels.iter().map(|p| p.height()).max().unwrap_or(0) + bar;
    let mut result = canvas(width, height);

    let mut x = 0;
    let mut text = TextBar::new(width, bar);
    for (panel, label) in panels.iter().zip(labels) {
        text.draw(label, (x + 4) as f32, 4.0, font_size);
        paste(&mut result, panel, x, bar);
        x += panel.width() + GAP;
    }
    text.blend_onto(&mut result);
    result
}

/// The whole image at the path, padded to the given size and scaled to fit.
///
/// The padding shows up only when the two renderings disagree about the size
/// of the map. The scaling keeps the overview openable: it is there to show
/// where in the map to look, while the crops show the differences themselves.
fn whole(path: &Path, height: u32, width: u32, limit: u32) -> Result<RgbImage, String> {
    let mut image = open_image(path)?;
    let scale = if limit > 0 && width > limit { limit as f64 / width as f64 } else { 1.0 };
    if scale < 1.0 {
        image = image::imageops::resize(
            &image,
            (python_round(image.width() as f64 * scale) as u32).max(1),
            (python_round(image.height() as f64 * scale) as u32).max(1),
            image::imageops::FilterType::Lanczos3,
        );
    }

    let target = (
        (python_round(width as f64 * scale) as u32).max(1),
        (python_round(height as f64 * scale) as u32).max(1),
    );
    if (image.width(), image.height()) != target {
        let mut padded = canvas(target.0, target.1);
        paste(&mut padded, &image, 0, 0);
        image = padded;
    }
    Ok(image)
}

/// Opens an image as red, green and blue.
///
/// Where a rendering has an alpha channel it is dropped rather than
/// composited, which is what Pillow's own conversion to RGB does, and what a
/// straight per-channel comparison of the two buffers sees.
///
/// Without limits: the `image` crate refuses by default to decode past 512MiB,
/// a guard against a hostile file, which a map rendered by this project's own
/// `benchmark` and `create_benchmark` is not. A real map at a fine resolution
/// clears that on its own — the images compared here are not.
pub fn open_image(path: &Path) -> Result<RgbImage, String> {
    let mut reader =
        image::ImageReader::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    reader.no_limits();
    reader
        .decode()
        .map(|image| image.to_rgb8())
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Writes the differing regions, enlarged, as expected, predicted and diff.
///
/// The regions come out worst first, and the file names are numbered in that
/// order and padded to the same width, so that they stay in it whatever
/// lists them.
fn write_crops(
    expected: &RgbImage,
    predicted: &RgbImage,
    mask: &Mask,
    folder: &Path,
    options: &Options,
) -> Result<(), String> {
    let size = options.crop_size.min(mask.height).min(mask.width);
    let regions = worst_regions(mask, options.crops, options.crop_size);
    let digits = regions.len().to_string().len();

    for (number, &(x, y, differing)) in regions.iter().enumerate() {
        let mut panels = vec![
            crop(expected, x, y, size),
            crop(predicted, x, y, size),
            crop_of_mask(mask, x, y, size),
        ];

        let zoom = (options.zoom / size.max(1)).max(1);
        if zoom > 1 {
            panels = panels.iter().map(|panel| magnify(panel, zoom)).collect();
        }

        let labels = [
            "expected".to_string(),
            "predicted".to_string(),
            format!("diff  {differing} real px"),
        ];
        let name = format!("crop_{:0digits$}_{x}x{y}.png", number + 1, digits = digits);
        compose(&panels, &labels).save(folder.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Writes the mask as a black, orange and red image.
///
/// A palette image, because the full size difference of a large map is a
/// hundred megapixels of no more than three colours — and because the
/// mask's bytes are already the palette indices.
fn write_diff(mask: &Mask, folder: &Path) -> Result<(), String> {
    let file = std::fs::File::create(folder.join("diff.png")).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), mask.width, mask.height);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(vec![
        0, 0, 0,
        ANTIALIASING_COLOUR[0], ANTIALIASING_COLOUR[1], ANTIALIASING_COLOUR[2],
        DIFFERENCE_COLOUR[0], DIFFERENCE_COLOUR[1], DIFFERENCE_COLOUR[2],
    ]);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&mask.bits).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

/// Whether either image of the pair is too large, decoded, to be worth
/// reading a second time for [`write_side_by_side`]'s overview.
fn too_big_for_overview(sizes: [(u32, u32); 2]) -> bool {
    sizes.iter().any(|&(w, h)| w as u64 * h as u64 * 3 > MAX_OVERVIEW_BYTES)
}

/// Writes both images whole, expected on the left, predicted on the right.
///
/// The images are read again here, rather than kept from the comparison: a
/// panel of a large map is hundreds of megabytes, and holding both maps and
/// both panels at once is what would decide the memory needed.
fn write_side_by_side(
    paths: [&Path; 2],
    sizes: [(u32, u32); 2],
    height: u32,
    width: u32,
    folder: &Path,
    options: &Options,
) -> Result<(), String> {
    let mut panels = Vec::new();
    let mut labels = Vec::new();
    for ((path, size), label) in paths.iter().zip(sizes).zip(["expected", "predicted"]) {
        let panel = whole(path, height, width, options.overview)?;
        let scaled = if panel.width() == width {
            String::new()
        } else {
            format!(", shown at {}x{}", panel.width(), panel.height())
        };
        labels.push(format!("{label}  {}x{}{scaled}", size.0, size.1));
        panels.push(panel);
    }
    compose(&panels, &labels).save(folder.join("side_by_side.png")).map_err(|e| e.to_string())
}

/// Compares the renderings in `predictions` against the reference images in
/// `expected`, writing a report per differing pair into `output`.
///
/// Draws a progress bar as it goes and says nothing else: what each pair
/// measured comes back in the report's rows, for the caller to write out as
/// a table.
pub fn compare(expected: &Path, predictions: &Path, output: &Path, options: &Options) -> Result<Report, String> {
    for folder in [expected, predictions] {
        if !folder.is_dir() {
            return Err(format!("No such directory: {}", folder.display()));
        }
    }

    let mut references: Vec<PathBuf> = std::fs::read_dir(expected)
        .map_err(|e| format!("cannot read {}: {e}", expected.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "png"))
        .filter(|path| {
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(&options.filter))
        })
        .collect();
    references.sort();

    if references.is_empty() {
        return Err(format!("No reference images in {}", expected.display()));
    }

    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let mut report = Report { total: references.len(), ..Default::default() };
    let mut progress = crate::progress::Progress::new("Comparing", references.len());
    for reference in &references {
        let file_name = reference.file_name().unwrap();
        let name = reference.file_stem().unwrap().to_string_lossy().to_string();
        let rendering = predictions.join(file_name);
        let folder = output.join(&name);
        if !rendering.exists() {
            report.missing.push(name);
            progress.tick();
            continue;
        }

        let expected_image = open_image(reference)?;
        let predicted_image = open_image(&rendering)?;
        let measured = difference_mask(
            &expected_image,
            &predicted_image,
            options.tolerance,
            options.keep_antialiasing,
        );
        let mask = measured.mask;
        let real = mask.count_real();

        report.rows.push(Row {
            name: name.clone(),
            differing: mask.count() as f64 / mask.size() as f64,
            real: real as f64 / mask.size() as f64,
            largest: measured.largest,
            error: measured.error,
        });

        // A pair which used to differ but no longer does must not leave a
        // stale report behind.
        if folder.exists() {
            std::fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;
        }
        if !mask.any() {
            report.identical += 1;
            progress.tick();
            continue;
        }
        // A pair whose every difference is an edge both renderers drew has
        // nothing in it to look at, so it gets no folder — which is what
        // makes what is left in differences/ a short list worth opening.
        if real == 0 {
            report.antialiasing_only += 1;
            progress.tick();
            continue;
        }
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;

        write_crops(&expected_image, &predicted_image, &mask, &folder, options)?;
        write_diff(&mask, &folder)?;

        // Nothing below needs the maps in memory, and the overview is about
        // to want the room they take.
        let sizes = [
            (expected_image.width(), expected_image.height()),
            (predicted_image.width(), predicted_image.height()),
        ];
        drop(expected_image);
        drop(predicted_image);
        if too_big_for_overview(sizes) {
            report.no_overview.push(name.clone());
        } else {
            write_side_by_side([reference, &rendering], sizes, mask.height, mask.width, &folder, options)?;
        }

        report.differing += 1;
        progress.tick();
    }
    progress.finish();
    Ok(report)
}

/// The results table: one line per compared pair, worst first.
///
/// Written to a file rather than to the terminal because it is as long as the
/// suite is, and because it is the thing worth keeping from a run.
pub fn write_results(report: &Report, title: &str, options: &Options, path: &Path) -> Result<(), String> {
    let mut text = String::new();
    text.push_str(&format!("Benchmark results: {title}\n\n"));
    text.push_str(&format!(
        "{} image{} compared: {} identical, {} antialiasing only, {} differing",
        report.total,
        if report.total == 1 { "" } else { "s" },
        report.identical,
        report.antialiasing_only,
        report.differing
    ));
    if report.missing.is_empty() {
        text.push('\n');
    } else {
        text.push_str(&format!(", {} not rendered\n", report.missing.len()));
    }
    text.push('\n');
    text.push_str(&crate::report::paragraph(&format!(
        "The error of a pixel is its difference summed over red, green and blue, so it runs \
         from 0 to 765. A pixel is wrong when its error is above the tolerance, which was {}.",
        options.tolerance
    )));
    text.push_str(&crate::report::paragraph(
        "\"wrong\" is the share of wrong pixels over the union of the two images, so where the \
         two disagree about the size of the map, every pixel outside the overlap counts as wrong. \
         It splits into \"antialiasing\" and \"real\".",
    ));
    if options.keep_antialiasing {
        text.push_str(&crate::report::paragraph(
            "This run was made with --keep-antialiasing, so nothing was put down to antialiasing \
             and every wrong pixel is counted as real.",
        ));
    } else {
        text.push_str(&crate::report::paragraph(
            "A pixel counts as \"antialiasing\" when both renderings have a colour step in the 3x3 \
             window around it, i.e. when both of them drew an edge there. Along an edge a pixel's \
             colour is a blend of what lies on either side, mixed in proportion to how much of the \
             pixel the shape covers, and two rasterizers work that coverage out differently. \
             Requiring it of both is what carries the weight: where only one has an edge, one of \
             them drew something the other did not. Everything else is \"real\": a missing symbol, \
             an extra mark, a shape well out of place, an area filled in the wrong colour, or a \
             rendering of the wrong size.",
        ));
        text.push_str(&crate::report::paragraph(
            "That 3x3 window is also the limit of what this can tell apart. A shape drawn under a \
             pixel out of place has exactly the signature of a coverage disagreement, and no local \
             test can separate the two, so up to a pixel of positional disagreement is forgiven — \
             which is about as far as either rasterizer spreads an edge. By the same token a \
             colour error confined to the pixels of an edge is forgiven, though over any area more \
             than a pixel wide the interior has no edge in either image and is still reported. A \
             systematic bias, say every edge coming out lighter than Qt draws it, would also land \
             under antialiasing; that is what the \"wrong\" column is still there for. Run with \
             --keep-antialiasing to see every difference again.",
        ));
    }
    text.push_str(&crate::report::paragraph(
        "The table is sorted by \"real\", worst first, so the maps worth looking at are at the \
         top. Only those with a real difference get a folder in differences/.",
    ));
    text.push_str(&crate::report::paragraph(&format!(
        "\"mean error of wrong px\" averages the error over the wrong pixels alone — both kinds, \
         so it mostly says how far apart the two rasterizers are along an edge — and comes with \
         its standard deviation. Every pixel at or below the tolerance is left out of it{}. An \
         average over all pixels would instead mostly measure how much blank paper a map has, \
         since the two renderings agree on all of it. Pixels outside the overlap count as wrong \
         above but have no error to measure, so they are not in this average either. It reads \
         \"n/a\" where no pixel was wrong.",
        // At the default tolerance those are exactly the pixels which match.
        if options.tolerance == 0 { ", which at a tolerance of 0 means every pixel that matches exactly" } else { "" }
    )));

    // Worst first, by the real differences rather than by all of them: the
    // top of the table is then the maps worth looking at. Ties fall back on
    // the total and then on the name, so two runs of the same suite list
    // them in the same order.
    let mut ordered: Vec<&Row> = report.rows.iter().collect();
    let worst_first = |a: f64, b: f64| b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal);
    ordered.sort_by(|a, b| {
        worst_first(a.real, b.real)
            .then_with(|| worst_first(a.differing, b.differing))
            .then_with(|| a.name.cmp(&b.name))
    });

    // The cells are built first so that every column is exactly as wide as
    // the widest thing in it, header included, and the last one is not padded
    // at all — trailing spaces on a map name are only there to be deleted.
    let cells: Vec<[String; 6]> = ordered
        .iter()
        .map(|row| {
            [
                format!("{:.4}%", 100.0 * row.real),
                format!("{:.4}%", 100.0 * (row.differing - row.real)),
                format!("{:.4}%", 100.0 * row.differing),
                row.largest.to_string(),
                match row.error {
                    Some((mean, deviation)) => format!("{mean:.2} ± {deviation:.2}"),
                    None => "n/a".to_string(),
                },
                row.name.clone(),
            ]
        })
        .collect();

    let headings = ["real", "antialiasing", "wrong", "largest", "mean error of wrong px", "map"];
    let mut widths = headings.map(str::len);
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }

    let last = headings.len() - 1;
    let mut line = |row: &[String; 6]| {
        let columns: Vec<String> = row
            .iter()
            .zip(widths)
            .enumerate()
            // Numbers right, the name left, and nothing padded after it.
            .map(|(column, (cell, width))| {
                if column == last { cell.clone() } else { format!("{cell:>width$}") }
            })
            .collect();
        text.push_str(columns.join("  ").trim_end());
        text.push('\n');
    };
    line(&headings.map(str::to_string));
    line(&widths.map(|width| "-".repeat(width)));
    for row in &cells {
        line(row);
    }

    if !report.missing.is_empty() {
        text.push_str("\nNot rendered, so not compared:\n");
        for name in &report.missing {
            text.push_str(&format!("  {name}\n"));
        }
    }

    if !report.no_overview.is_empty() {
        text.push_str("\nToo large for a side_by_side.png; only the crops were written:\n");
        for name in &report.no_overview {
            text.push_str(&format!("  {name}\n"));
        }
    }

    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// The label bars
// ---------------------------------------------------------------------------

/// The best available font, never failing.
///
/// The candidates are tried in a fixed order, DejaVu first, so that two
/// machines with the same fonts installed label their reports identically —
/// the labels are part of an image people compare across runs, and a report
/// which suddenly switches typeface reads as though something changed.
fn label_font() -> Option<&'static (Vec<u8>, u32)> {
    use std::sync::OnceLock;
    static FONT: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
    FONT.get_or_init(|| {
        let candidates = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "DejaVuSansMono.ttf",
            "DejaVuSans.ttf",
        ];
        for candidate in candidates {
            if let Ok(data) = std::fs::read(candidate) {
                return Some((data, 0));
            }
        }
        // No DejaVu: any monospace face the system has is better than no
        // labels at all.
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let id = db.query(&fontdb::Query { families: &[fontdb::Family::Monospace], ..Default::default() })?;
        db.with_face_data(id, |data, index| (data.to_vec(), index))
    })
    .as_ref()
}

/// Turns a glyph outline (font design units, y pointing up) into a
/// `tiny-skia` path at the pen position.
struct GlyphOutline {
    builder: PathBuilder,
    scale: f32,
    x: f32,
    baseline: f32,
}

impl GlyphOutline {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x * self.scale, self.baseline - y * self.scale)
    }
}

impl rustybuzz::ttf_parser::OutlineBuilder for GlyphOutline {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.builder.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.builder.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.map(x1, y1);
        let (x, y) = self.map(x, y);
        self.builder.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.map(x1, y1);
        let (x2, y2) = self.map(x2, y2);
        let (x, y) = self.map(x, y);
        self.builder.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

/// The grey bar along the top of a composed image, and the text on it.
///
/// The glyphs are collected into one buffer and blended over the finished
/// canvas at the end, so that a composed image is still a plain RGB image
/// rather than an RGBA one that would have to be flattened again.
struct TextBar {
    pixmap: Option<Pixmap>,
}

impl TextBar {
    fn new(width: u32, height: u32) -> Self {
        TextBar { pixmap: Pixmap::new(width.max(1), height.max(1)) }
    }

    /// Draws `text` with its left edge at `x` and its ascender line at `y`,
    /// which is where Pillow's default text anchor puts them.
    fn draw(&mut self, text: &str, x: f32, y: f32, size: u32) {
        let Some(pixmap) = self.pixmap.as_mut() else { return };
        let Some((data, index)) = label_font() else { return };
        let Some(face) = rustybuzz::Face::from_slice(data, *index) else { return };

        let units_per_em = face.units_per_em() as f32;
        if units_per_em <= 0.0 {
            return;
        }
        let scale = size as f32 / units_per_em;
        // FreeType, which is what Pillow measures with, reports the ascender
        // rounded up to a whole pixel.
        let baseline = y + (face.ascender() as f32 * scale).ceil();

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_direction(rustybuzz::Direction::LeftToRight);
        let shaped = rustybuzz::shape(&face, &[], buffer);

        let mut builder = GlyphOutline { builder: PathBuilder::new(), scale, x, baseline };
        for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            face.outline_glyph(rustybuzz::ttf_parser::GlyphId(info.glyph_id as u16), &mut builder);
            // Whole pixel advances, as an unhinted FreeType layout also uses.
            builder.x += (position.x_advance as f32 * scale).round();
        }

        if let Some(path) = builder.builder.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(LABEL_FOREGROUND[0], LABEL_FOREGROUND[1], LABEL_FOREGROUND[2], 255);
            paint.anti_alias = true;
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    /// Blends the collected glyphs over the top of `target`.
    fn blend_onto(self, target: &mut RgbImage) {
        let Some(pixmap) = self.pixmap else { return };
        for y in 0..pixmap.height().min(target.height()) {
            for x in 0..pixmap.width().min(target.width()) {
                let source = pixmap.pixel(x, y).unwrap();
                let alpha = source.alpha() as u32;
                if alpha == 0 {
                    continue;
                }
                // tiny-skia's buffer is premultiplied, which is already the
                // source term of an over-blend.
                let destination = target.get_pixel_mut(x, y);
                for (channel, value) in [source.red(), source.green(), source.blue()].iter().enumerate() {
                    let under = destination[channel] as u32 * (255 - alpha);
                    destination[channel] = (*value as u32 + (under + 127) / 255).min(255) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, colour: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(width, height, Rgb(colour))
    }

    #[test]
    fn identical_images_have_an_empty_mask() {
        let measured = difference_mask(&image(4, 4, [1, 2, 3]), &image(4, 4, [1, 2, 3]), 0, true);
        assert!(!measured.mask.any());
        assert_eq!(measured.largest, 0);
        // No wrong pixel, so there is nothing to average.
        assert_eq!(measured.error, None);
    }

    #[test]
    fn a_difference_within_the_tolerance_does_not_count() {
        let a = image(4, 4, [100, 100, 100]);
        let b = image(4, 4, [110, 110, 110]);
        let measured = difference_mask(&a, &b, 30, true);
        assert_eq!(measured.largest, 30);
        assert!(!measured.mask.any(), "a distance of exactly the tolerance is not a difference");
        // Nothing is wrong at that tolerance, so nothing is averaged either,
        // even though every pixel does differ a little.
        assert_eq!(measured.error, None);

        let measured = difference_mask(&a, &b, 29, true);
        assert_eq!(measured.mask.count(), 16);
        // Now every pixel is wrong, all by the same amount.
        assert_eq!(measured.error, Some((30.0, 0.0)));
    }

    #[test]
    fn the_mean_error_leaves_out_the_pixels_which_are_not_wrong() {
        let mut a = image(4, 1, [0, 0, 0]);
        let b = image(4, 1, [0, 0, 0]);
        // Two of the four pixels agree; the other two are off by 100 and 300.
        a.put_pixel(2, 0, Rgb([0, 0, 100]));
        a.put_pixel(3, 0, Rgb([100, 100, 100]));
        let measured = difference_mask(&a, &b, 0, true);
        assert_eq!(measured.largest, 300);
        assert_eq!(measured.mask.count(), 2);
        // The mean of 100 and 300, not of 0, 0, 100 and 300 (which is 100).
        assert_eq!(measured.error, Some((200.0, 100.0)));
    }

    #[test]
    fn a_raised_tolerance_moves_pixels_out_of_the_average_too() {
        let mut a = image(4, 1, [0, 0, 0]);
        let b = image(4, 1, [0, 0, 0]);
        a.put_pixel(2, 0, Rgb([0, 0, 100]));
        a.put_pixel(3, 0, Rgb([100, 100, 100]));
        // The pixel off by 100 is now within tolerance, so only the 300 is left.
        let measured = difference_mask(&a, &b, 100, true);
        assert_eq!(measured.mask.count(), 1);
        assert_eq!(measured.error, Some((300.0, 0.0)));
    }

    /// A one-pixel-wide edge from `left` to `right`, with the middle pixel
    /// blended between them by `coverage`.
    fn edge(coverage: f64, left: u8, right: u8) -> RgbImage {
        let blend = (left as f64 + (right as f64 - left as f64) * coverage).round() as u8;
        let mut result = RgbImage::new(5, 3);
        for y in 0..3 {
            for x in 0..5 {
                let value = match x {
                    0 | 1 => left,
                    2 => blend,
                    _ => right,
                };
                result.put_pixel(x, y, Rgb([value; 3]));
            }
        }
        result
    }

    #[test]
    fn the_same_edge_covered_differently_is_antialiasing() {
        // Both drew the edge in the same place; they only disagree about how
        // much of the middle pixel the shape covers.
        let a = edge(0.4, 255, 0);
        let b = edge(0.6, 255, 0);
        let measured = difference_mask(&a, &b, 0, false);
        assert_eq!(measured.mask.count(), 3, "the middle column differs");
        assert_eq!(measured.mask.count_real(), 0, "and it is all antialiasing");
        // Turning the classification off counts the very same pixels as real.
        assert_eq!(difference_mask(&a, &b, 0, true).mask.count_real(), 3);
    }

    #[test]
    fn an_area_filled_in_the_wrong_colour_is_a_real_difference() {
        // The right-hand half comes out green instead of black. The column
        // against the edge is forgiven — both images have an edge there, and
        // a colour error one pixel wide is exactly what cannot be told from a
        // coverage disagreement — but the interior, where neither image has
        // an edge, is reported.
        let a = edge(0.5, 255, 0);
        let mut b = a.clone();
        for y in 0..3 {
            b.put_pixel(3, y, Rgb([0, 200, 0]));
            b.put_pixel(4, y, Rgb([0, 200, 0]));
        }
        let measured = difference_mask(&a, &b, 0, false);
        assert_eq!(measured.mask.count(), 6, "both green columns differ");
        assert_eq!(measured.mask.count_real(), 3, "and the far one has no edge to hide behind");
    }

    #[test]
    fn a_shape_displaced_beyond_the_window_is_a_real_difference() {
        // A dot on blank paper, moved four pixels: at the old and the new
        // place the other image has nothing but white nearby.
        let mut a = image(9, 3, [255, 255, 255]);
        let mut b = image(9, 3, [255, 255, 255]);
        a.put_pixel(1, 1, Rgb([0, 0, 0]));
        b.put_pixel(5, 1, Rgb([0, 0, 0]));
        let measured = difference_mask(&a, &b, 0, false);
        assert_eq!(measured.mask.count_real(), 2, "the dot is missing here and extra there");
    }

    #[test]
    fn a_shape_displaced_inside_the_window_is_forgiven() {
        // The same dot moved one pixel: indistinguishable from a coverage
        // disagreement by any local test, and documented as such.
        let mut a = image(9, 3, [255, 255, 255]);
        let mut b = image(9, 3, [255, 255, 255]);
        a.put_pixel(4, 1, Rgb([0, 0, 0]));
        b.put_pixel(5, 1, Rgb([0, 0, 0]));
        let measured = difference_mask(&a, &b, 0, false);
        assert_eq!(measured.mask.count(), 2);
        assert_eq!(measured.mask.count_real(), 0);
    }

    #[test]
    fn a_sub_pixel_feature_no_window_can_see_is_still_antialiasing() {
        // The case the old, stricter test got wrong: a road casing, where a
        // gap of white paper narrower than a pixel makes the edge pixel a mix
        // of black, asphalt and a white that appears nowhere nearby. Both
        // renderings agree on the black and the asphalt and differ only on
        // how the sliver of white lands.
        let (black, asphalt) = (Rgb([0, 0, 0]), Rgb([232, 167, 116]));
        let mut a = RgbImage::new(3, 3);
        let mut b = RgbImage::new(3, 3);
        for x in 0..3 {
            a.put_pixel(x, 0, black);
            b.put_pixel(x, 0, black);
            a.put_pixel(x, 1, Rgb([199, 161, 130]));
            b.put_pixel(x, 1, Rgb([179, 147, 122]));
            a.put_pixel(x, 2, asphalt);
            b.put_pixel(x, 2, asphalt);
        }
        let measured = difference_mask(&a, &b, DEFAULT_TOLERANCE, false);
        assert_eq!(measured.mask.count(), 3, "the blended row differs");
        assert_eq!(measured.mask.count_real(), 0, "and both drew the same edge there");
    }

    #[test]
    fn a_flat_area_off_by_a_rounding_step_is_within_the_default_tolerance() {
        // One unit per channel is 3 summed, which is what DEFAULT_TOLERANCE
        // forgives; there is no edge anywhere, so nothing could explain it.
        let a = image(4, 4, [120, 130, 140]);
        let b = image(4, 4, [121, 131, 141]);
        assert!(!difference_mask(&a, &b, DEFAULT_TOLERANCE, false).mask.any());
        let measured = difference_mask(&a, &b, DEFAULT_TOLERANCE - 1, false);
        assert_eq!(measured.mask.count_real(), 16, "a flat area is never antialiasing");
    }

    #[test]
    fn pixels_outside_the_overlap_all_differ() {
        let measured = difference_mask(&image(4, 4, [0, 0, 0]), &image(6, 2, [0, 0, 0]), 0, true);
        assert_eq!((measured.mask.width, measured.mask.height), (6, 4));
        // The 4x2 overlap agrees, everything else is outside it.
        assert_eq!(measured.mask.count(), 6 * 4 - 4 * 2);
        // Those pixels count as wrong but have no error to measure, so the
        // average has nothing in it.
        assert_eq!(measured.error, None);
    }

    #[test]
    fn regions_are_taken_worst_first_and_do_not_overlap() {
        let mut expected = image(200, 200, [0, 0, 0]);
        let predicted = image(200, 200, [0, 0, 0]);
        // A dense cluster near (150, 150) and a single pixel at (10, 10).
        for y in 150..160 {
            for x in 150..160 {
                expected.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        expected.put_pixel(10, 10, Rgb([255, 255, 255]));

        let mask = difference_mask(&expected, &predicted, 0, true).mask;
        let regions = worst_regions(&mask, 0, 64);
        assert_eq!(regions.len(), 2);
        assert!(regions[0].2 > regions[1].2);
        // Every differing pixel ends up inside a region.
        assert_eq!(regions.iter().map(|r| r.2).sum::<u64>(), 101);
        // The regions are clamped into the image and do not overlap.
        for &(x, y, _) in &regions {
            assert!(x + 64 <= 200 && y + 64 <= 200);
        }
        let (a, b) = (regions[0], regions[1]);
        assert!(a.0 + 64 <= b.0 || b.0 + 64 <= a.0 || a.1 + 64 <= b.1 || b.1 + 64 <= a.1);
    }

    #[test]
    fn a_crop_is_padded_where_the_image_ends() {
        let source = image(10, 10, [255, 255, 255]);
        let cropped = crop(&source, 6, 6, 8);
        assert_eq!(cropped.dimensions(), (8, 8));
        assert_eq!(*cropped.get_pixel(0, 0), Rgb([255, 255, 255]));
        assert_eq!(*cropped.get_pixel(7, 7), LABEL_BACKGROUND);
    }

    #[test]
    fn python_rounds_a_tie_to_the_even_number() {
        assert_eq!(python_round(0.5), 0.0);
        assert_eq!(python_round(1.5), 2.0);
        assert_eq!(python_round(2.5), 2.0);
        assert_eq!(python_round(-0.5), 0.0);
        assert_eq!(python_round(2.4), 2.0);
    }

    #[test]
    fn a_pair_within_the_overview_budget_is_not_too_big() {
        // A little under 512MiB of RGB8 each, comfortably below the limit.
        assert!(!too_big_for_overview([(10_000, 17_000), (10_000, 17_000)]));
    }

    #[test]
    fn either_image_past_the_overview_budget_makes_the_pair_too_big() {
        // Just over 512MiB of RGB8 — the real map that motivated this was
        // 11292x15972, well past it.
        assert!(too_big_for_overview([(10_000, 17_900), (1, 1)]));
        assert!(too_big_for_overview([(1, 1), (10_000, 17_900)]));
    }
}
