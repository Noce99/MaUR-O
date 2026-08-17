//! The folder `generate_maps_dataset` wrote, as something burn can train on.
//!
//! A dataset on disk is three folders and a `classes.json` — see
//! [`maur_o::dataset`]. What a learner wants is a stream of batched tensors,
//! and the two are further apart than they look, for one reason: a map is
//! drawn at 1650 by 1650 pixels and a U-Net at that size is a tensor of two
//! and a half million pixels per image per level. So an item here is not a
//! map. It is a **crop** of one, [`CROP`] pixels square, taken at random from
//! somewhere inside it, and a batch is a handful of those.
//!
//! Cropping is not only a way of fitting in memory: it is most of what stands
//! in for having more maps. A dataset of a hundred maps is a hundred
//! pictures, and a hundred maps cropped at random is as many different
//! pictures as are ever asked for, each of them showing a boundary or two
//! from an angle the last one did not.
//!
//! # What an item is
//!
//! [`MapCrop`] is one crop, in the plain vectors a tensor is built from:
//!
//! * the image, `[3, CROP, CROP]`, RGB scaled to `[-1, 1]`;
//! * the class of each pixel, `[CROP, CROP]`, as an index into the symbol
//!   list of `classes.json` — with the white frame given a class of its own,
//!   the last one, rather than the `0xFFFF` a file writes;
//! * the sine and the cosine of each pixel's angle, `[2, CROP, CROP]`.
//!
//! The label on disk is `H, W, C` and everything here is `C, H, W`, which is
//! what burn's convolutions read. The order changes here, once, while the
//! batch is built.
//!
//! # What is read when
//!
//! [`MapDataset::get`] reads one map's PNG and one map's labels, crops both,
//! and throws the rest away. That is a whole 1650 by 1650 image decoded for
//! every crop, which is honest work rather than clever: it keeps nothing in
//! memory between items, so a dataset of any size trains in the same
//! footprint, and the decode overlaps with the last batch's arithmetic as
//! long as the loader has workers to do it in. Caching decoded maps would be
//! the first thing to try if the loader ever became the slow half.
//!
//! # Why nothing here returns `None`
//!
//! Because burn would believe it. Its loader walks a dataset as
//! `while let Some(item) = dataset.get(index)`, so an item which declines to
//! exist does not get skipped — it **ends the epoch**, silently, wherever it
//! happens to be. One map that failed to decode would quietly cut a run short
//! at whatever fraction of the data came before it, and a failure at the
//! first index would leave the epoch empty and every metric dividing by no
//! pixels.
//!
//! So the checking happens in [`MapDataset::load`], which reads the header of
//! every labels file — thirty-two bytes each — and refuses a folder whose
//! images and labels disagree, before a learner is ever built. After that
//! there is nothing left for [`MapDataset::get`] to decline about, and what
//! it does instead of returning `None` is panic, loudly, naming the file:
//! something changed on disk mid-run, and a short epoch nobody was told about
//! is the worst of the ways to report it.

use std::path::{Path, PathBuf};

use burn::data::dataloader::batcher::Batcher;
use burn::data::dataset::Dataset;
use burn::prelude::Backend;
use burn::tensor::{Int, Tensor, TensorData};

use maur_o::dataset::{GROUND_TRUTH_FOLDER, IMAGES_FOLDER};
use maur_o::ground_truth::{GroundTruth, BACKGROUND};
use maur_o::random::Random;

/// How many pixels square a crop is.
///
/// Divides by `2^DEPTH` — a U-Net halves what it is given four times over —
/// and at the three pixels per meter a dataset is drawn at, 256 of them is
/// eighty-five meters of ground: more than half a cell, so a crop usually
/// holds a boundary and both of the symbols it separates.
pub const CROP: usize = 256;

/// How many crops are taken from each map in one pass over the dataset.
///
/// A map is read to make one crop, so this is also how much of that reading
/// is amortized: eight crops of a map is eight items and eight decodes, but
/// it is eight items which need only as many maps as the folder holds.
pub const DEFAULT_CROPS_PER_MAP: usize = 8;

/// One crop of one map: what the network is shown, and what it should say.
#[derive(Clone, Debug)]
pub struct MapCrop {
    /// The image, `[3, CROP, CROP]` row by row, RGB scaled to `[-1, 1]`.
    pub image: Vec<f32>,
    /// The class of each pixel, `[CROP, CROP]`: an opaque area's place in
    /// `classes.json`, or `classes` itself for the white frame.
    pub class: Vec<i32>,
    /// The sine and the cosine of each pixel's angle, `[2, CROP, CROP]`, and
    /// `(0, 0)` where there is no angle to give.
    pub angle: Vec<f32>,
    /// How many pixels square this crop is.
    pub size: usize,
}

/// The maps of a dataset folder, cropped.
#[derive(Debug)]
pub struct MapDataset {
    /// The image and the labels of each map, in the order the folder sorts.
    maps: Vec<(PathBuf, PathBuf)>,
    /// How many pixels the maps are across and down — the same for all of
    /// them, which [`MapDataset::load`] is where it is checked.
    size: (usize, usize),
    /// How many opaque areas the labels were written for.
    classes: usize,
    /// How many crops one pass takes from each map.
    crops_per_map: usize,
    /// How many pixels square a crop is.
    crop: usize,
    /// What the crop positions come out of. A dataset is asked for the same
    /// item more than once — every epoch, for one — and an item which
    /// changed between two asks would make a validation score mean nothing,
    /// so the position of crop `n` is decided by `n` and this, and not by
    /// when it was asked for.
    seed: u64,
}

impl MapDataset {
    /// Every map of the dataset in `folder`, which is what
    /// `generate_maps_dataset` wrote: the images in `images/` and the labels
    /// in `gt/`, paired by name.
    ///
    /// A map whose image has no labels beside it is left out rather than
    /// complained about: that is what a folder generated without
    /// `--just-opaque-areas` looks like, and the message for it belongs where
    /// the folder is empty.
    pub fn load(folder: &Path, seed: u64) -> Result<MapDataset, String> {
        let images = folder.join(IMAGES_FOLDER);
        let labels = folder.join(GROUND_TRUTH_FOLDER);

        let mut entries: Vec<PathBuf> = std::fs::read_dir(&images)
            .map_err(|e| format!("cannot read {}: {e}", images.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|suffix| suffix == "png"))
            .collect();
        // The folder is one map per name and the names sort the way they were
        // generated; a directory listing is in whatever order the filesystem
        // felt like.
        entries.sort();

        let maps: Vec<(PathBuf, PathBuf)> = entries
            .into_iter()
            .filter_map(|image| {
                let label = labels.join(image.file_stem()?).with_extension("bin");
                label.is_file().then_some((image, label))
            })
            .collect();

        if maps.is_empty() {
            return Err(format!(
                "{} holds no map with labels beside it: a dataset is trained on what \
                 generate_maps_dataset --just-opaque-areas writes, which is {IMAGES_FOLDER}/ and \
                 {GROUND_TRUTH_FOLDER}/ under one set of names",
                folder.display()
            ));
        }

        // Every labels file's header, which is thirty-two bytes of a sixteen
        // megabyte file. A folder whose maps disagree about their size or
        // about how many symbols there are is a folder two runs of the
        // generator were emptied into, and the time to say so is now — not
        // eight minutes into an epoch, from inside a loader thread.
        let (mut size, mut classes) = (None, None);
        for (image, label) in &maps {
            let (height, width, holds) = GroundTruth::size(label)?;
            let (height, width) = (height as usize, width as usize);

            if *size.get_or_insert((width, height)) != (width, height) {
                let (was_width, was_height) = size.expect("just set");
                return Err(format!(
                    "{} is {width}x{height} where the maps before it are {was_width}x{was_height}: \
                     a dataset trains as one batch of one shape, so its maps have to be one size",
                    label.display(),
                ));
            }
            if *classes.get_or_insert(holds) != holds {
                return Err(format!(
                    "{} was labelled for {holds} symbols where the maps before it hold {}: these \
                     are two datasets in one folder",
                    label.display(),
                    classes.expect("just set"),
                ));
            }
            // The picture and the answer have to be the same picture, and a
            // PNG says how big it is in its own header.
            let (image_width, image_height) = image_size(image)?;
            if (image_width, image_height) != (width, height) {
                return Err(format!(
                    "{} is {image_width}x{image_height} and {} labels {width}x{height}: the \
                     answer is not an answer to that picture",
                    image.display(),
                    label.display(),
                ));
            }
        }

        Ok(MapDataset {
            maps,
            size: size.expect("there is at least one map"),
            classes: classes.expect("there is at least one map"),
            crops_per_map: DEFAULT_CROPS_PER_MAP,
            crop: CROP,
            seed,
        })
    }

    /// How many crops to take from each map in one pass.
    pub fn with_crops_per_map(mut self, crops: usize) -> MapDataset {
        self.crops_per_map = crops.max(1);
        self
    }

    /// How many pixels square a crop is, which cannot be more than the maps
    /// are: there is no crop of a picture bigger than the picture.
    pub fn with_crop(mut self, crop: usize) -> Result<MapDataset, String> {
        let (width, height) = self.size;
        if crop > width || crop > height {
            return Err(format!(
                "a crop of {crop} pixels does not fit in a map of {width}x{height}"
            ));
        }
        self.crop = crop.max(16);
        Ok(self)
    }

    /// How many maps the folder held.
    pub fn maps(&self) -> usize {
        self.maps.len()
    }

    /// How many pixels square this dataset's crops are.
    pub fn crop(&self) -> usize {
        self.crop
    }

    /// How many opaque areas the labels were written for, which is what a
    /// network read off them has to have a channel each for.
    pub fn classes(&self) -> usize {
        self.classes
    }

    /// Splits the maps in two, the first `share` of them for training and the
    /// rest for validation.
    ///
    /// By map rather than by crop: two crops of one map overlap as often as
    /// not, and a validation score measured on a crop of a map the network
    /// trained on is a score for how well it remembers, not for how well it
    /// reads.
    pub fn split(self, share: f64) -> (MapDataset, MapDataset) {
        // At least one map each way wherever there are two to divide.
        let at = ((self.maps.len() as f64 * share).round() as usize)
            .clamp(1, self.maps.len().saturating_sub(1).max(1));
        let (train, valid) = self.maps.split_at(at);
        let part = |maps: &[(PathBuf, PathBuf)], seed: u64| MapDataset {
            maps: maps.to_vec(),
            size: self.size,
            classes: self.classes,
            crops_per_map: self.crops_per_map,
            crop: self.crop,
            seed,
        };
        // Different seeds, so the two halves do not crop at the same corners.
        (
            part(train, self.seed),
            part(valid, self.seed.wrapping_add(0x5eed)),
        )
    }
}

/// How big a PNG is, out of its header rather than by decoding it.
///
/// `image` reads only as far as it has to for this, which is what makes
/// checking a folder of them cheap enough to do before every run.
fn image_size(path: &Path) -> Result<(usize, usize), String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok((width as usize, height as usize))
}

impl Dataset<MapCrop> for MapDataset {
    fn len(&self) -> usize {
        self.maps.len() * self.crops_per_map
    }

    /// The `index`-th crop.
    ///
    /// `None` only past the end of the dataset, which is what burn reads it
    /// as. Anything else that could go wrong here went wrong in
    /// [`MapDataset::load`] instead, or is a file which changed under a
    /// running loader — and that panics, because the alternative burn offers
    /// is an epoch which stops early and says nothing.
    fn get(&self, index: usize) -> Option<MapCrop> {
        let (image_path, label_path) = self.maps.get(index / self.crops_per_map)?;

        let truth = GroundTruth::read(label_path)
            .unwrap_or_else(|e| panic!("cannot read the labels mid-run: {e}"));
        let image = image::open(image_path)
            .unwrap_or_else(|e| panic!("cannot read {} mid-run: {e}", image_path.display()))
            .to_rgb8();
        let (width, height) = (image.width() as usize, image.height() as usize);
        assert_eq!(
            (width, height),
            self.size,
            "{} changed size under a running loader",
            image_path.display(),
        );

        // The same index gives the same corner however often it is asked for.
        let mut random = Random::from_seed(self.seed.wrapping_add(index as u64));
        let left = random.below(width - self.crop + 1);
        let top = random.below(height - self.crop + 1);

        let pixels = self.crop * self.crop;
        let mut rgb = vec![0.0; 3 * pixels];
        let mut class = vec![0; pixels];
        let mut angle = vec![0.0; 2 * pixels];
        // The frame is the last class rather than the file's 0xFFFF: a
        // cross-entropy counts classes from nothing upwards and has no room
        // for a sentinel.
        let frame = truth.classes as i32;

        for row in 0..self.crop {
            for column in 0..self.crop {
                let at = row * self.crop + column;
                let from = (top + row) * width + left + column;

                let pixel = image.get_pixel((left + column) as u32, (top + row) as u32);
                for (channel, &value) in pixel.0.iter().enumerate() {
                    // Nought to 255 becomes -1 to 1, which is where a network
                    // with a symmetric activation would rather be given its
                    // input.
                    rgb[channel * pixels + at] = value as f32 / 127.5 - 1.0;
                }

                class[at] = match truth.class_of[from] {
                    BACKGROUND => frame,
                    symbol => symbol as i32,
                };
                let (sin, cos) = truth.sin_cos(from);
                angle[at] = sin;
                angle[pixels + at] = cos;
            }
        }

        Some(MapCrop {
            image: rgb,
            class,
            angle,
            size: self.crop,
        })
    }
}

/// A batch of crops, as the tensors a training step takes.
#[derive(Clone, Debug)]
pub struct MapBatch<B: Backend> {
    /// The images, `[batch, 3, crop, crop]`.
    pub image: Tensor<B, 4>,
    /// The class of every pixel, `[batch, crop, crop]`, the frame's included.
    pub class: Tensor<B, 3, Int>,
    /// The sine and the cosine of every pixel's angle,
    /// `[batch, 2, crop, crop]`.
    pub angle: Tensor<B, 4>,
}

/// Stacks [`MapCrop`]s into a [`MapBatch`].
#[derive(Clone, Default)]
pub struct MapBatcher;

impl<B: Backend> Batcher<B, MapCrop, MapBatch<B>> for MapBatcher {
    fn batch(&self, items: Vec<MapCrop>, device: &B::Device) -> MapBatch<B> {
        let size = items.first().map(|item| item.size).unwrap_or(CROP);
        let batch = items.len();

        // One tensor per field rather than one per item then stacked: the
        // crops are all the same size, so the batch is the items' own
        // vectors laid end to end, which is a copy and no more.
        let mut image = Vec::with_capacity(batch * 3 * size * size);
        let mut class = Vec::with_capacity(batch * size * size);
        let mut angle = Vec::with_capacity(batch * 2 * size * size);
        for item in items {
            image.extend_from_slice(&item.image);
            class.extend_from_slice(&item.class);
            angle.extend_from_slice(&item.angle);
        }

        MapBatch {
            image: Tensor::from_data(TensorData::new(image, [batch, 3, size, size]), device),
            class: Tensor::from_data(TensorData::new(class, [batch, size, size]), device),
            angle: Tensor::from_data(TensorData::new(angle, [batch, 2, size, size]), device),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn::backend::ndarray::{NdArray, NdArrayDevice};

    type TestBackend = NdArray<f32>;

    fn crop_of(value: f32, class: i32, size: usize) -> MapCrop {
        MapCrop {
            image: vec![value; 3 * size * size],
            class: vec![class; size * size],
            angle: vec![value; 2 * size * size],
            size,
        }
    }

    #[test]
    fn a_batch_is_the_crops_end_to_end() {
        let device = NdArrayDevice::default();
        let batch: MapBatch<TestBackend> =
            MapBatcher.batch(vec![crop_of(1.0, 0, 4), crop_of(-1.0, 2, 4)], &device);

        assert_eq!(batch.image.dims(), [2, 3, 4, 4]);
        assert_eq!(batch.class.dims(), [2, 4, 4]);
        assert_eq!(batch.angle.dims(), [2, 2, 4, 4]);

        // The first crop's numbers came first, and the second's after them.
        let image = batch.image.into_data();
        let values = image.as_slice::<f32>().expect("floats");
        assert_eq!(values[0], 1.0);
        assert_eq!(values[3 * 4 * 4], -1.0);
    }

    /// A folder with nothing to train on says so, rather than handing back a
    /// dataset of no items for a learner to divide by.
    #[test]
    fn a_folder_without_labels_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        std::fs::create_dir_all(dir.path().join(IMAGES_FOLDER)).expect("the images folder");
        let error = MapDataset::load(dir.path(), 0).expect_err("nothing to train on");
        assert!(error.contains("no map with labels"), "{error}");
    }
}
