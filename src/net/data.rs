//! The folder `generate_maps_dataset` wrote, as something burn can train on.
//!
//! A dataset on disk is three folders and a `classes.json` — see
//! [`crate::dataset`]. What a learner wants is a stream of batched tensors,
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
//! * the sine and the cosine of each pixel's angle, `[2, CROP, CROP]` —
//!   **folded** by however many turns of that pixel's symbol look exactly
//!   alike, which is what makes a pattern drawn at four indistinguishable
//!   angles one target instead of four (see
//!   [`crate::ground_truth::GroundTruth::sin_cos_folded`], and
//!   `crate::net::predict::resolve_angles` for the way back);
//! * and which map it was cut out of, and where — read by nothing which
//!   trains, and there for anything which looks at a crop rather than
//!   learning from it.
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
//!
//! # A map with no `gt/` beside it
//!
//! A generated map's objects *are* its answer: every one of them is a cell
//! filled with one opaque area of the symbol set, at the angle its pattern
//! was turned to, so the labels [`crate::ground_truth::GroundTruth::rasterize`]
//! writes to `gt/` can be rasterized straight back out of `maps/<name>.omap`
//! instead, whenever that `.bin` was left out of the folder — dropped to save
//! the disk it takes, or a dataset moved without it. [`MapDataset::load`]
//! keeps a map like that rather than leaving it out, and
//! [`MapDataset::get`] computes its labels afresh every time they are asked
//! for, the same as it decodes the image afresh: honest work rather than
//! clever, and one fewer reason a dataset has to be regenerated whole to be
//! trained on again.
//!
//! What that costs beyond reading a `.bin` is a map file to parse and a
//! handful of cells to rasterize — cheap next to decoding the image beside
//! it — and one thing a `.bin` never needed: the resolution the image was
//! drawn at, which a map file says nothing about and which
//! [`crate::dataset::resolution_of`] reads from `classes.json` instead.

use std::path::{Path, PathBuf};

use burn::data::dataloader::batcher::Batcher;
use burn::data::dataset::Dataset;
use burn::prelude::Backend;
use burn::tensor::{Int, Tensor, TensorData};

use crate::dataset::{resolution_of, Classes, CLASSES_FILE, GROUND_TRUTH_FOLDER, IMAGES_FOLDER, MAPS_FOLDER};
use crate::ground_truth::{GroundTruth, BACKGROUND};
use crate::random::Random;
use crate::symbol_kinds::Catalogue;
use crate::xml_reader::read_xml_map;

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
    /// The name of the map it was cut out of, without its suffix.
    ///
    /// Nothing in training reads this: it is here for anything looking at a
    /// crop rather than learning from one — `show_batches` names the map a
    /// crop came from, and a crop which looks wrong is worth being able to
    /// find in `images/`.
    pub map: String,
    /// Which pixel of that map the crop's left edge is at. Negative where the
    /// crop hangs off the edge — see [`MapDataset::with_overhang`].
    ///
    /// With [`MapCrop::top`], what puts the crop back where it came from:
    /// the fill patterns of a map are drawn against the map's own ground, so
    /// a crop rendered anywhere else comes out with its dots in the wrong
    /// places even when every class is right.
    pub left: isize,
    /// Which pixel of that map the crop's top edge is at.
    pub top: isize,
}

/// Where one map's labels come from.
#[derive(Clone, Debug)]
enum Labels {
    /// Written to `gt/<name>.bin` by `generate_maps_dataset`, and read back
    /// as they are.
    File(PathBuf),
    /// Not on disk: rasterized afresh from `maps/<name>.omap` every time they
    /// are asked for, the same as the image beside them is decoded afresh.
    /// See [`crate::net::data`]'s module documentation.
    FromMap(PathBuf),
}

/// The maps of a dataset folder, cropped.
#[derive(Debug)]
pub struct MapDataset {
    /// The image and the labels of each map, in the order the folder sorts.
    maps: Vec<(PathBuf, Labels)>,
    /// How many pixels the maps are across and down — the same for all of
    /// them, which [`MapDataset::load`] is where it is checked.
    size: (usize, usize),
    /// How many opaque areas the labels were written for.
    classes: usize,
    /// The resolution the images were drawn at, in pixels per meter of
    /// ground — what rasterizing a [`Labels::FromMap`] needs to land on the
    /// same pixel grid as the image beside it, and read once for the whole
    /// dataset since a map file says nothing about it. `None` where every
    /// map's labels are already on disk and nothing ever asks.
    resolution: Option<f64>,
    /// How many crops one pass takes from each map.
    crops_per_map: usize,
    /// How many pixels square a crop is.
    crop: usize,
    /// How far a crop may hang off the edge of the map, in pixels.
    ///
    /// Nought keeps every crop wholly inside, which is the obvious thing to
    /// do and quietly wrong: a crop's corner is drawn uniformly, so the pixel
    /// it lands on is that corner plus an offset — the sum of two uniform
    /// draws, which piles up in the middle of a map and thins towards its
    /// edges. The white frame is a third of a picture and a seventh of what
    /// 256-pixel crops show of it, so the network is trained on a mixture the
    /// map does not hold and `image_to_map` does not show it.
    ///
    /// Letting a crop hang off and filling what is past the edge with white
    /// is what evens that out, and it is honest rather than a trick: past the
    /// edge of a map really is white paper, which is what the frame is, and
    /// `net::predict::tile_tensor` already pads its tiles with white for the
    /// same reason. Half the crop makes the crop's *centre* uniform over the
    /// map, and so every pixel of the map equally likely — see
    /// [`MapDataset::with_overhang`].
    overhang: usize,
    /// How many turns of each class look exactly alike, in the order the
    /// classes are numbered — [`crate::symbol_kinds::pattern_symmetry`] of
    /// each opaque area of the symbol set. What the angle of a pixel is
    /// folded by before it is handed over, and all ones for a dataset whose
    /// `classes.json` names no symbol set to read them from.
    symmetry: Vec<u32>,
    /// What the crop positions come out of. A dataset is asked for the same
    /// item more than once — every epoch, for one — and an item which
    /// changed between two asks would make a validation score mean nothing,
    /// so the position of crop `n` is decided by `n` and this, and not by
    /// when it was asked for.
    seed: u64,
}

impl MapDataset {
    /// Every map of the dataset in `folder`, which is what
    /// `generate_maps_dataset` wrote: the images in `images/`, the maps in
    /// `maps/`, and the labels in `gt/`, paired by name.
    ///
    /// A map whose labels are not in `gt/` is not left out for that alone: it
    /// is kept wherever `maps/<name>.omap` is there to rasterize them from
    /// instead, and only left out where neither is. That is what a folder
    /// generated without `--just-opaque-areas` looks like — the map draws
    /// more than its ground cover, so there is no answer to make up for it
    /// either — and the message for it belongs where the folder is empty.
    pub fn load(folder: &Path, seed: u64) -> Result<MapDataset, String> {
        let images = folder.join(IMAGES_FOLDER);
        let labels = folder.join(GROUND_TRUTH_FOLDER);
        let maps_folder = folder.join(MAPS_FOLDER);

        let mut entries: Vec<PathBuf> = std::fs::read_dir(&images)
            .map_err(|e| format!("cannot read {}: {e}", images.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|suffix| suffix == "png"))
            .collect();
        // The folder is one map per name and the names sort the way they were
        // generated; a directory listing is in whatever order the filesystem
        // felt like.
        entries.sort();

        let maps: Vec<(PathBuf, Labels)> = entries
            .into_iter()
            .filter_map(|image| {
                let stem = image.file_stem()?;
                let label = labels.join(stem).with_extension("bin");
                if label.is_file() {
                    return Some((image, Labels::File(label)));
                }
                let map = maps_folder.join(stem).with_extension("omap");
                map.is_file().then_some((image, Labels::FromMap(map)))
            })
            .collect();

        if maps.is_empty() {
            return Err(format!(
                "{} holds no map with labels beside it, on disk or in its own map file: a \
                 dataset is trained on what generate_maps_dataset --just-opaque-areas writes, \
                 which is {IMAGES_FOLDER}/ and either {GROUND_TRUTH_FOLDER}/ or {MAPS_FOLDER}/ \
                 under one set of names",
                folder.display()
            ));
        }

        // The resolution a `Labels::FromMap` is rasterized at, read once
        // rather than per map: it is one dataset-wide setting, and reading it
        // before there is a map which needs it means a folder missing
        // `classes.json` is refused here rather than eight minutes into an
        // epoch, from inside a loader thread.
        let mut resolution = None;
        if maps.iter().any(|(_, l)| matches!(l, Labels::FromMap(_))) {
            resolution = Some(resolution_of(folder)?);
        }

        // Every labels file's header, which is thirty-two bytes of a sixteen
        // megabyte file — or, for a map computing its own, its catalogue of
        // opaque areas. A folder whose maps disagree about their size or
        // about how many symbols there are is a folder two runs of the
        // generator were emptied into, and the time to say so is now.
        let (mut size, mut classes) = (None, None);
        for (image, label) in &maps {
            let (width, height, holds) = match label {
                Labels::File(label) => {
                    let (height, width, holds) = GroundTruth::size(label)?;
                    (width as usize, height as usize, holds)
                }
                Labels::FromMap(map) => {
                    let (mut parsed, _) = read_xml_map(map)?;
                    parsed.resolve_references();
                    let holds = Catalogue::of(&parsed).opaque_areas.len();
                    let (width, height) = image_size(image)?;
                    (width, height, holds)
                }
            };

            if *size.get_or_insert((width, height)) != (width, height) {
                let (was_width, was_height) = size.expect("just set");
                return Err(format!(
                    "{} is {width}x{height} where the maps before it are {was_width}x{was_height}: \
                     a dataset trains as one batch of one shape, so its maps have to be one size",
                    image.display(),
                ));
            }
            if *classes.get_or_insert(holds) != holds {
                return Err(format!(
                    "{} was labelled for {holds} symbols where the maps before it hold {}: these \
                     are two datasets in one folder",
                    image.display(),
                    classes.expect("just set"),
                ));
            }
            // The picture and the answer have to be the same picture, and a
            // PNG says how big it is in its own header. A `Labels::FromMap`
            // took its size from the picture already, so it agrees by
            // construction; only a `Labels::File` can disagree.
            if let Labels::File(label) = label {
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
        }

        let classes = classes.expect("there is at least one map");
        Ok(MapDataset {
            maps,
            size: size.expect("there is at least one map"),
            classes,
            resolution,
            crops_per_map: DEFAULT_CROPS_PER_MAP,
            crop: CROP,
            overhang: 0,
            symmetry: symmetry_of(folder, classes),
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
        // A crop of another size wants its overhang checked again against it.
        self.overhang = self.overhang.min(self.crop.saturating_sub(1));
        Ok(self)
    }

    /// How far a crop may hang off the edge of the map, padded with the
    /// white a map is printed on — see the field, which is where the reason
    /// is.
    ///
    /// Clamped below the crop, since a crop entirely off the map is a crop of
    /// nothing at all. Half the crop is the figure with a meaning: it makes
    /// the crop's centre uniform over the map, so every pixel of the map is
    /// equally likely to be trained on, which is the mixture a whole picture
    /// through `image_to_map` shows.
    pub fn with_overhang(mut self, overhang: usize) -> MapDataset {
        self.overhang = overhang.min(self.crop.saturating_sub(1));
        self
    }

    /// How many maps the folder held.
    pub fn maps(&self) -> usize {
        self.maps.len()
    }

    /// The pictures of those maps, in the order the folder sorts — which is
    /// the order they were generated in.
    ///
    /// The crops are what a network is trained on; these are the whole
    /// images the crops were cut from, for anything which wants to put one
    /// through the network as it stands. `crate::net::image_valid` reads
    /// the first few of a validation split back into maps, epoch by epoch.
    pub fn pictures(&self) -> impl Iterator<Item = &Path> {
        self.maps.iter().map(|(image, _)| image.as_path())
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

    /// What share of the pixels **a crop shows** each class holds, the frame
    /// last: `classes + 1` figures which sum to one.
    ///
    /// Over the crops rather than over the whole maps, and that is the whole
    /// point of it. A network trained on crops is never shown a map, and the
    /// two do not hold the same mixture: a crop is placed at a corner drawn
    /// uniformly, so the pixel it lands on is that corner plus an offset —
    /// the sum of two uniform draws, which piles up in the middle of a map
    /// and thins out towards its edges. Anything living at the edge is seen
    /// less often than it covers. On a generated dataset the white frame is a
    /// third of every picture and a seventh of what 256-pixel crops show, and
    /// counted the other way it comes out as the commonest class in the
    /// dataset when it is really the rarest thing the network is shown.
    ///
    /// The crops counted are the very crops [`MapDataset::get`] would serve —
    /// same seed, same corners — so this is a census of the training set and
    /// not a model of it.
    ///
    /// Off `sample` maps rather than all of them, taken at an even stride
    /// through the folder so that a dataset generated in some order is not
    /// counted from one end of it. A map whose labels are rasterized afresh
    /// rather than read from `gt/` costs what rasterizing it costs, which is
    /// what the stride is here to bound.
    ///
    /// A class no sampled crop held comes back as nought, and what to do
    /// about that belongs to whoever asked: this counts, it does not smooth.
    pub fn class_balance(&self, sample: usize) -> Result<Vec<f64>, String> {
        let mut counted = vec![0u64; self.classes + 1];
        if self.maps.is_empty() {
            return Ok(counted.iter().map(|_| 0.0).collect());
        }

        // At least one map, and never a stride which walks off the end.
        let wanted = sample.clamp(1, self.maps.len());
        let stride = self.maps.len() / wanted;
        let (width, height) = self.size;
        let size = self.crop.min(width).min(height);

        for map in (0..self.maps.len()).step_by(stride.max(1)).take(wanted) {
            let (_, labels) = &self.maps[map];
            let truth = self.labels(labels)?;

            // The crops this map would be asked for, at the corners `get`
            // works out for those same item indices.
            for crop in 0..self.crops_per_map {
                let index = map * self.crops_per_map + crop;
                let mut random = Random::from_seed(self.seed.wrapping_add(index as u64));
                let (left, top) = self.corner(&mut random, width, height);

                for row in 0..size {
                    for column in 0..size {
                        // Off the map is white paper, which is the frame --
                        // and the frame is the last class.
                        let at = match inside(left + column as isize, top + row as isize, width, height)
                            .map(|from| truth.class_of[from])
                        {
                            None | Some(BACKGROUND) => self.classes,
                            Some(symbol) => symbol as usize,
                        };
                        counted[at] += 1;
                    }
                }
            }
        }

        let total: u64 = counted.iter().sum();
        if total == 0 {
            return Err("the sampled labels hold no pixels at all".to_string());
        }
        Ok(counted
            .into_iter()
            .map(|count| count as f64 / total as f64)
            .collect())
    }

    /// The corner a crop is taken from, which may be off the map by up to
    /// [`MapDataset::overhang`] in either direction.
    ///
    /// One place rather than two, because [`MapDataset::class_balance`] has
    /// to count the very crops [`MapDataset::get`] serves — a census of the
    /// training set is no use if it is drawn from somewhere else.
    fn corner(&self, random: &mut Random, width: usize, height: usize) -> (isize, isize) {
        // A crop no larger than the map it comes out of: `with_crop` refuses
        // one that does not fit, but a dataset asked for a balance before
        // that has the default still on it.
        let size = self.crop.min(width).min(height);
        let overhang = self.overhang.min(size.saturating_sub(1));
        let span = |length: usize| length + 2 * overhang - size + 1;
        (
            random.below(span(width)) as isize - overhang as isize,
            random.below(span(height)) as isize - overhang as isize,
        )
    }

    /// One map's labels, however they are kept — read from `gt/`, or
    /// rasterized from the map itself.
    fn labels(&self, labels: &Labels) -> Result<GroundTruth, String> {
        match labels {
            Labels::File(path) => GroundTruth::read(path),
            Labels::FromMap(path) => {
                let (width, height) = self.size;
                let resolution = self
                    .resolution
                    .expect("set in load whenever a map's labels are computed from it");
                GroundTruth::from_map(path, width as u32, height as u32, resolution)
            }
        }
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
        let part = |maps: &[(PathBuf, Labels)], seed: u64| MapDataset {
            maps: maps.to_vec(),
            size: self.size,
            classes: self.classes,
            resolution: self.resolution,
            crops_per_map: self.crops_per_map,
            crop: self.crop,
            overhang: self.overhang,
            symmetry: self.symmetry.clone(),
            seed,
        };
        // Different seeds, so the two halves do not crop at the same corners.
        (
            part(train, self.seed),
            part(valid, self.seed.wrapping_add(0x5eed)),
        )
    }
}

/// Where in the map `truth.class_of` a pixel of the crop comes from, and
/// `None` for one which is off the map altogether.
fn inside(x: isize, y: isize, width: usize, height: usize) -> Option<usize> {
    let (inside_x, inside_y) = (
        (0..width as isize).contains(&x),
        (0..height as isize).contains(&y),
    );
    (inside_x && inside_y).then(|| y as usize * width + x as usize)
}

/// How many turns of each of a dataset's classes look exactly alike, in the
/// order the classes are numbered.
///
/// Read off the symbol set `classes.json` names, which is the only place it
/// is written down: a label file holds an angle per pixel and says nothing
/// about which of them a picture could tell apart. See
/// [`crate::symbol_kinds::pattern_symmetry`] for what is being counted.
///
/// All ones — which folds nothing, and leaves the angle exactly as the labels
/// hold it — for a dataset generated before the symbol set was copied in
/// beside them, or one whose symbol set will not parse. That is the old
/// behaviour rather than an error: a run can still be had from it, and the
/// warning says what it will cost.
fn symmetry_of(folder: &Path, classes: usize) -> Vec<u32> {
    let plain = vec![1u32; classes];
    let complain = |what: String| {
        eprintln!(
            "Warning: {what}, so the pattern angles are left unfolded. A symbol whose pattern              looks the same at several angles cannot be learned that way -- see              maur_o::symbol_kinds::pattern_symmetry."
        );
    };

    let notes = folder.join(CLASSES_FILE);
    let Ok(read) = Classes::read(&notes) else {
        complain(format!("{} cannot be read", notes.display()));
        return plain;
    };
    let Some(named) = read.symbol_set else {
        complain(format!("{} names no symbol set", notes.display()));
        return plain;
    };
    let set = folder.join(named);
    let Ok((mut map, _)) = read_xml_map(&set) else {
        complain(format!("{} cannot be read", set.display()));
        return plain;
    };
    map.resolve_references();

    let areas = Catalogue::of(&map).opaque_areas;
    if areas.len() != classes {
        complain(format!(
            "{} holds {} opaque areas and the labels were written for {classes}",
            set.display(),
            areas.len(),
        ));
        return plain;
    }
    areas.iter().map(|area| area.symmetry).collect()
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
        let (image_path, labels) = self.maps.get(index / self.crops_per_map)?;

        let truth = self
            .labels(labels)
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
        let (left, top) = self.corner(&mut random, width, height);

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
                let (x, y) = (left + column as isize, top + row as isize);

                // Past the edge of the map is the white paper a map is
                // printed on, which is the same thing the frame around it is
                // -- see `MapDataset::overhang`. Only reachable at all where
                // an overhang was asked for.
                let Some(from) = inside(x, y, width, height) else {
                    for channel in 0..3 {
                        // 255 on the scale below, which is white.
                        rgb[channel * pixels + at] = 1.0;
                    }
                    class[at] = frame;
                    continue;
                };

                let pixel = image.get_pixel(x as u32, y as u32);
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
                // Folded by however many turns of this class look alike,
                // which is what makes the angle a thing there is one answer
                // to -- see `GroundTruth::sin_cos_folded`.
                // The frame's class is `BACKGROUND`, which is past the end
                // of the list and comes back as one -- and it has no angle to
                // fold either way.
                let order = self
                    .symmetry
                    .get(truth.class_of[from] as usize)
                    .copied()
                    .unwrap_or(1);
                let (sin, cos) = truth.sin_cos_folded(from, order);
                angle[at] = sin;
                angle[pixels + at] = cos;
            }
        }

        Some(MapCrop {
            image: rgb,
            class,
            angle,
            size: self.crop,
            map: image_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            left,
            top,
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
            map: "map".to_string(),
            left: 0,
            top: 0,
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

    /// A crop allowed to hang off the edge sees the map's rim as often as the
    /// map holds it, and one held wholly inside does not.
    ///
    /// This is the whole case for [`MapDataset::with_overhang`], as a number:
    /// count how often the outermost band of a map turns up in the crops, and
    /// compare it against how much of the map that band actually is.
    #[test]
    fn an_overhang_lets_a_crop_see_the_edge_of_the_map() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("dataset");
        let settings = crate::dataset::Settings {
            layout_size: 3,
            cell_size: 60,
            maps: 4,
            just_opaque_areas: true,
            resolution: 2.0,
            frame: 30.0,
            ..crate::dataset::Settings::default()
        };
        crate::dataset::create_dataset(
            Path::new("tests/data/turning_patterns.xmap"),
            &folder,
            &settings,
            |_, _| {},
        )
        .expect("the dataset generates");

        // The frame is the outermost band, and the only class which lives at
        // the edge of a map: how much of the crops it holds is the measure.
        let balance = |overhang: usize| -> f64 {
            let dataset = MapDataset::load(&folder, 0)
                .expect("the dataset loads")
                .with_crop(64)
                .expect("a crop which fits")
                .with_crops_per_map(24)
                .with_overhang(overhang);
            let shares = dataset.class_balance(8).expect("a balance");
            shares[dataset.classes()]
        };

        let held_in = balance(0);
        let hanging_over = balance(32);
        assert!(
            hanging_over > held_in * 1.2,
            "a crop held inside sees the frame {:.1} per cent of the time and one allowed to \
             hang off sees it {:.1}, which is not the point of the option",
            100.0 * held_in,
            100.0 * hanging_over,
        );
    }

    /// An overhang is clamped below the crop however it is asked for: a crop
    /// entirely off the map is a crop of nothing at all, and a crop resized
    /// afterwards has its overhang checked again against the new size.
    #[test]
    fn an_overhang_never_takes_the_crop_off_the_map() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("dataset");
        let settings = crate::dataset::Settings {
            layout_size: 2,
            cell_size: 40,
            maps: 2,
            just_opaque_areas: true,
            resolution: 2.0,
            frame: 10.0,
            ..crate::dataset::Settings::default()
        };
        crate::dataset::create_dataset(
            Path::new("tests/data/turning_patterns.xmap"),
            &folder,
            &settings,
            |_, _| {},
        )
        .expect("the dataset generates");

        let dataset = MapDataset::load(&folder, 0)
            .expect("the dataset loads")
            .with_crop(64)
            .expect("a crop which fits")
            .with_overhang(usize::MAX)
            .with_crop(32)
            .expect("a smaller crop");

        // Whatever was asked for, no crop is taken from entirely off the map:
        // the corner stays within a crop's width of it on either side, so
        // some of the picture is always in there. It may be almost all white
        // -- that is what an overhang of a crop less one means -- but it is
        // never a crop of nowhere.
        for index in 0..dataset.len().min(8) {
            let crop = dataset.get(index).expect("a crop");
            assert_eq!(crop.image.len(), 3 * crop.size * crop.size);
            assert_eq!(crop.class.len(), crop.size * crop.size);
            let reach = crop.size as isize;
            assert!(
                crop.left > -reach && crop.top > -reach,
                "crop {index} starts at {},{} which is a whole crop off the map",
                crop.left,
                crop.top,
            );
        }
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

    /// A map whose `gt/` label is missing is not left out: its labels are
    /// computed straight from `maps/<name>.omap` instead, and every crop
    /// taken from it comes out the same as when the label was read off disk.
    #[test]
    fn a_missing_label_is_computed_from_the_map_beside_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("dataset");
        let settings = crate::dataset::Settings {
            layout_size: 2,
            cell_size: 20,
            maps: 2,
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

        // Read while the labels are still on disk, before they are taken
        // away: `get` panics on a file which disappears under it, same as it
        // would mid-run.
        // A crop smaller than the default 256, since this dataset's images
        // are only 100 pixels square.
        let with_labels = MapDataset::load(&folder, 7)
            .expect("the labels are on disk")
            .with_crop(32)
            .expect("32 fits in a 100 pixel map");
        let from_disk: Vec<MapCrop> = (0..with_labels.len())
            .map(|index| with_labels.get(index).expect("an item"))
            .collect();

        std::fs::remove_dir_all(folder.join(GROUND_TRUTH_FOLDER)).expect("gt/ removed");
        let without_labels = MapDataset::load(&folder, 7)
            .expect("the labels come from the maps instead")
            .with_crop(32)
            .expect("32 fits in a 100 pixel map");

        assert_eq!(without_labels.maps(), with_labels.maps());
        assert_eq!(without_labels.classes(), with_labels.classes());
        assert_eq!(without_labels.len(), from_disk.len());

        for (index, from_disk) in from_disk.iter().enumerate() {
            let from_map = without_labels.get(index).expect("an item");
            assert_eq!(from_map.class, from_disk.class, "crop {index}");
            // Not bit for bit: a map file keeps a rotation to six
            // significant digits, so a sine or cosine worked out from it
            // carries a hair less precision than one read off the `.bin`.
            for (at, (&got, &wanted)) in from_map.angle.iter().zip(&from_disk.angle).enumerate() {
                assert!(
                    (got - wanted).abs() < 1e-4,
                    "crop {index} angle {at}: {got} from the map, {wanted} from disk",
                );
            }
        }
    }

    /// The balance is a share per class and the frame's is one of them, so it
    /// sums to one however the ground fell -- and the frame of a dataset
    /// drawn with a wide border is a large share of it rather than a
    /// rounding.
    #[test]
    fn the_class_balance_is_a_share_of_the_pixels_each() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("dataset");
        let settings = crate::dataset::Settings {
            layout_size: 2,
            cell_size: 20,
            maps: 4,
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
        let shares = dataset.class_balance(4).expect("the labels are counted");

        assert_eq!(shares.len(), dataset.classes() + 1);
        let total: f64 = shares.iter().sum();
        assert!((total - 1.0).abs() < 1e-9, "the shares sum to {total}");
        assert!(
            shares.iter().all(|&share| (0.0..=1.0).contains(&share)),
            "{shares:?}",
        );
        // The frame is the last of them, and a five meter border round a
        // forty meter map is not nothing.
        assert!(shares[dataset.classes()] > 0.0, "{shares:?}");
    }

    /// Counting a sample means counting some of the maps, not the first of
    /// them: a dataset generated in some order would otherwise be read from
    /// one end. A sample of one still counts one whole map.
    #[test]
    fn a_sample_of_the_maps_is_still_a_balance() {
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
        for sample in [1, 2, 3, 99] {
            let shares = dataset
                .class_balance(sample)
                .expect("the labels are counted");
            let total: f64 = shares.iter().sum();
            assert!(
                (total - 1.0).abs() < 1e-9,
                "a sample of {sample} summed to {total}",
            );
        }
    }
}
