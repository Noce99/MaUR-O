//! Showing a run's work: the maps it reads back, epoch by epoch.
//!
//! `train/` and `valid/` say how a run is doing in numbers — a loss, a pixel
//! accuracy, an angle error. Numbers do not show you a map. This is the third
//! split, `image_valid/`, and what it holds is pictures: for each epoch, a
//! few of the dataset's own images put back through the network and out again
//! as maps, drawn, and set beside the originals.
//!
//! ```text
//! image_valid/007/map_013.omap   the map the network read out of the picture
//! image_valid/007/map_013.png    the picture, that map drawn, and the difference
//! ```
//!
//! Opened in order, the middle panel of those sheets is a run learning to
//! read a map, which is the one thing a column of losses cannot tell you.
//!
//! # Where it hooks in
//!
//! burn hands no callback the model, and `Learner::fit` is its only public
//! method. The one extension point which fires once per epoch, after that
//! epoch's checkpoint has gone to the checkpointer, is
//! [`EarlyStoppingStrategy`] — so this is one, and it always answers "do not
//! stop". That does mean **a run has one such slot and this takes it**: a
//! real early stopping strategy and this phase cannot both be registered as
//! things stand.
//!
//! The model comes from the checkpoint rather than from the hook, which never
//! sees one. That is also why the sheets lag by an epoch. burn's checkpointer
//! writes on a thread of its own over a rendezvous channel, so when the hook
//! for epoch `N` runs, epoch `N`'s file may still be going down; the writer is
//! serial, so accepting `N` proves `N - 1` is finished. The hook therefore
//! draws epoch `N - 1`, and [`ImageValidation::catch_up`] — run once `fit` has
//! returned and the writer thread has been joined — draws whatever is left.
//!
//! # What is kept
//!
//! Whatever `checkpoint/` keeps. After each epoch's sheets are written, the
//! folders are pruned to the epochs which still have a checkpoint: burn's
//! checkpointing strategy has already decided what the last few and the best
//! are, and mirroring its decision keeps the two folders in step without a
//! second policy to disagree with it. It also bounds the cost — a hundred
//! epochs would otherwise leave a thousand maps behind.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use burn::prelude::Backend;
use burn::train::metric::store::EventStoreClient;
use burn::train::EarlyStoppingStrategy;
use image::RgbImage;

use crate::differences::{compose, mask_image, open_image};
use crate::symbol_kinds::Entry;

use crate::net::predict::{load, read_back, ReadBackSettings};

/// The folder a run's sheets go in, beside `train/` and `valid/`.
pub const IMAGE_VALID: &str = "image_valid";

/// The prefix burn gives an epoch's folder, which is what this names its own
/// with so that the same renumbering tidies both.
const EPOCH_PREFIX: &str = "epoch-";

/// The default number of pictures to read back each epoch.
///
/// Three, because a picture is not cheap and an early epoch's is dear. What a
/// network says before it has learned anything is noise, and noise vectorizes
/// into as many objects as the grid has cells to spare: the first epoch of a
/// run of this dataset gives half a million objects per picture and some two
/// hundred megabytes of map, against a few dozen kilobytes once the network is
/// reading real regions. Ten pictures of that, over the handful of epochs
/// `checkpoint/` keeps, is a run whose sheets weigh more than the dataset it
/// trained on and an epoch which spends longer drawing than training.
///
/// Three is enough to see whether a run is learning to read a map, which is
/// what the sheets are for; `--image-valid` is there for a run which wants
/// more of them, and nought for one in a hurry.
pub const DEFAULT_IMAGE_VALID: usize = 3;

/// The pictures a run reads back each epoch, and everything it takes to do
/// it.
///
/// Built once, before training starts, and handed to the learner as its
/// early stopping strategy — see the module documentation for why that is
/// the hook.
pub struct ImageValidation<B: Backend> {
    /// The run folder: where the checkpoints are read from and the sheets
    /// written to.
    run: PathBuf,
    /// The pictures to read back, in the order they are drawn.
    pictures: Vec<PathBuf>,
    /// The symbol set the network's classes are places in.
    symbol_set: PathBuf,
    /// Its opaque areas, which is that list.
    symbols: Vec<Entry>,
    /// How to read a picture back, which is what `image_to_map` would do.
    settings: ReadBackSettings,
    /// Where the network is run.
    device: B::Device,
}

impl<B: Backend> ImageValidation<B> {
    /// The phase for a run, over `pictures`.
    ///
    /// `pictures` is already cut to the number asked for; an empty list is a
    /// phase which does nothing at all, which is what a count of nought
    /// means.
    pub fn new(
        run: &Path,
        pictures: Vec<PathBuf>,
        symbol_set: &Path,
        symbols: Vec<Entry>,
        settings: ReadBackSettings,
        device: B::Device,
    ) -> ImageValidation<B> {
        ImageValidation {
            run: run.to_path_buf(),
            pictures,
            symbol_set: symbol_set.to_path_buf(),
            symbols,
            settings,
            device,
        }
    }

    /// Whether there is anything to do at all.
    pub fn wanted(&self) -> bool {
        !self.pictures.is_empty()
    }

    /// Draws the sheets of one epoch, and prunes what the checkpoints no
    /// longer keep.
    ///
    /// An epoch whose checkpoint has already gone is skipped rather than
    /// complained about: it was not one of the ones worth keeping, and the
    /// weights to draw it from are not there.
    pub fn epoch(&self, epoch: usize) -> Result<(), String> {
        if !self.wanted() {
            return Ok(());
        }
        let weights = self.run.join("checkpoint").join(format!("model-{epoch}"));
        let file = weights.with_extension("mpk");
        if !file.is_file() {
            return Ok(());
        }

        let folder = self.run.join(IMAGE_VALID).join(epoch_name(epoch));
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        let (model, _) = load::<B>(&self.run, Some(&file), &self.device)?;

        for picture in &self.pictures {
            let stem = picture
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "map".to_string());
            let image = open_image(picture)?;
            let done = read_back(
                &model,
                &image,
                &self.symbol_set,
                &self.symbols,
                &folder.join(format!("{stem}.omap")),
                &self.settings,
                &self.device,
            )?;

            let (agree, _, _) = done.shares();
            let drawn = sheet(
                image,
                done.drawn,
                mask_image(&done.comparison.mask),
                [
                    stem.clone(),
                    format!("epoch {epoch}"),
                    format!("{:.2}% agree", 100.0 * agree),
                ],
            );
            let path = folder.join(format!("{stem}.png"));
            drawn
                .save(&path)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        self.prune()
    }

    /// Draws whatever is still missing, once the checkpoints are all on disk.
    ///
    /// The hook lags an epoch behind, so the last one is always outstanding
    /// when `fit` returns; a run which was interrupted may be missing more
    /// than that. Every epoch whose checkpoint survived and whose folder is
    /// not there yet is drawn here.
    pub fn catch_up(&self) -> Result<(), String> {
        if !self.wanted() {
            return Ok(());
        }
        for epoch in kept_epochs(&self.run.join("checkpoint")) {
            let folder = self.run.join(IMAGE_VALID).join(epoch_name(epoch));
            if !folder.is_dir() {
                self.epoch(epoch)?;
            }
        }
        self.prune()
    }

    /// Removes the folders of epochs the checkpoints no longer keep.
    fn prune(&self) -> Result<(), String> {
        let kept = kept_epochs(&self.run.join("checkpoint"));
        let sheets = self.run.join(IMAGE_VALID);
        let Ok(entries) = std::fs::read_dir(&sheets) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(epoch) = name
                .to_str()
                .and_then(|name| name.strip_prefix(EPOCH_PREFIX))
                .and_then(|epoch| epoch.parse::<usize>().ok())
            else {
                continue;
            };
            if !kept.contains(&epoch) {
                std::fs::remove_dir_all(entry.path())
                    .map_err(|e| format!("cannot remove {}: {e}", entry.path().display()))?;
            }
        }
        Ok(())
    }
}

/// Not a stopping strategy at all: the one place burn will call user code
/// once an epoch, with the epoch's number and after its checkpoint has been
/// handed over. It always answers that the run should carry on.
impl<B: Backend> EarlyStoppingStrategy for ImageValidation<B> {
    fn should_stop(&mut self, epoch: usize, _store: &EventStoreClient) -> bool {
        // The epoch before this one, whose checkpoint is certainly written —
        // see the module documentation. A failure here is a picture nobody
        // got, not a reason to throw away the training which produced it.
        if epoch > 1 {
            if let Err(message) = self.epoch(epoch - 1) {
                eprintln!("Warning: cannot draw epoch {}: {message}", epoch - 1);
            }
        }
        false
    }
}

/// What an epoch's folder is called while the run is going: burn's own name
/// for one, so that the renumbering which tidies `train/` tidies this too.
fn epoch_name(epoch: usize) -> String {
    format!("{EPOCH_PREFIX}{epoch}")
}

/// Which epochs the checkpoint folder still holds.
///
/// Both the shapes it takes: `model-7.mpk` while the run is going, and
/// `007/model.mpk` once the run has tidied itself up.
fn kept_epochs(checkpoint: &Path) -> BTreeSet<usize> {
    let mut epochs = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(checkpoint) else {
        return epochs;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let epoch = if entry.path().is_dir() {
            name.parse::<usize>().ok()
        } else {
            Path::new(name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.strip_prefix("model-"))
                .and_then(|epoch| epoch.parse::<usize>().ok())
        };
        if let Some(epoch) = epoch {
            epochs.insert(epoch);
        }
    }
    epochs
}

/// A picture, a map drawn from it and where the two disagree, side by side
/// with a caption over each.
///
/// Public so that anything else which wants the same sheet — a tool over a
/// run folder, say — makes the same one.
pub fn sheet(
    picture: RgbImage,
    drawn: RgbImage,
    difference: RgbImage,
    labels: [String; 3],
) -> RgbImage {
    compose(&[picture, drawn, difference], &labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkpoint folder in each of the two shapes it takes.
    fn checkpoints(at: &Path, epochs: &[usize], grouped: bool) {
        std::fs::create_dir_all(at).expect("the checkpoint folder");
        for &epoch in epochs {
            if grouped {
                let folder = at.join(format!("{epoch:03}"));
                std::fs::create_dir_all(&folder).expect("an epoch's folder");
                std::fs::write(folder.join("model.mpk"), []).expect("the weights");
            } else {
                std::fs::write(at.join(format!("model-{epoch}.mpk")), []).expect("the weights");
                std::fs::write(at.join(format!("optim-{epoch}.mpk")), []).expect("the optimizer");
            }
        }
    }

    #[test]
    fn the_epochs_a_checkpoint_folder_holds_are_read_from_either_shape() {
        let dir = tempfile::tempdir().expect("a temporary folder");

        let flat = dir.path().join("flat");
        checkpoints(&flat, &[7, 8, 9], false);
        assert_eq!(kept_epochs(&flat), [7, 8, 9].into_iter().collect());

        let grouped = dir.path().join("grouped");
        checkpoints(&grouped, &[7, 8, 9], true);
        assert_eq!(kept_epochs(&grouped), [7, 8, 9].into_iter().collect());

        // A folder which is not there is no epochs, not an error: a run of no
        // epochs never made one.
        assert!(kept_epochs(&dir.path().join("nothing")).is_empty());
    }

    /// The sheets follow the checkpoints: an epoch whose weights have been
    /// dropped loses its pictures too.
    #[test]
    fn pruning_keeps_the_epochs_the_checkpoints_keep() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let run = dir.path();
        checkpoints(&run.join("checkpoint"), &[3, 8, 9, 10], false);
        for epoch in 1..=10 {
            let folder = run.join(IMAGE_VALID).join(epoch_name(epoch));
            std::fs::create_dir_all(&folder).expect("an epoch's sheets");
            std::fs::write(folder.join("map_001.png"), []).expect("a sheet");
        }

        let phase: ImageValidation<burn::backend::NdArray> = ImageValidation::new(
            run,
            vec![PathBuf::from("map_001.png")],
            Path::new("symbols.omap"),
            Vec::new(),
            ReadBackSettings::of(64, 3.0, 10000),
            Default::default(),
        );
        phase.prune().expect("cannot prune");

        let mut left: Vec<String> = std::fs::read_dir(run.join(IMAGE_VALID))
            .expect("the folder")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["epoch-10", "epoch-3", "epoch-8", "epoch-9"]);
    }

    /// A phase over no pictures is a phase which does nothing, which is what
    /// a count of nought asks for.
    #[test]
    fn a_phase_over_no_pictures_does_nothing() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let phase: ImageValidation<burn::backend::NdArray> = ImageValidation::new(
            dir.path(),
            Vec::new(),
            Path::new("symbols.omap"),
            Vec::new(),
            ReadBackSettings::of(64, 3.0, 10000),
            Default::default(),
        );
        assert!(!phase.wanted());
        phase.epoch(1).expect("nothing to do");
        phase.catch_up().expect("nothing to do");
        assert!(!dir.path().join(IMAGE_VALID).exists());
    }

    /// A sheet is as wide as its panels and the gaps between them, and taller
    /// than the tallest by the caption bar.
    #[test]
    fn a_sheet_is_its_panels_side_by_side() {
        let panel = |w, h| RgbImage::new(w, h);
        let sheet = sheet(
            panel(40, 30),
            panel(40, 30),
            panel(40, 20),
            [
                "map_001".to_string(),
                "epoch 7".to_string(),
                "99%".to_string(),
            ],
        );
        assert_eq!(sheet.width(), 40 * 3 + 8 * 2);
        assert!(
            sheet.height() > 30,
            "the caption bar is above the panels, not over them"
        );
    }
}
