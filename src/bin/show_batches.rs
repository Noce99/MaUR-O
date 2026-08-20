//! Draws the batches a run would be trained on, instead of training on them.
//!
//! ```text
//! show_batches [OPTIONS] <dataset> [into]
//! ```
//!
//! Everything a network learns it learns from what [`maur_o::net::data`]
//! hands it, and nothing in a training run ever shows you that. A loss going
//! down says the network agrees with the labels; it does not say the labels
//! are right. This builds the crops exactly as `train` builds them — the same
//! dataset, the same split, the same crop size and the same seed, stacked by
//! the same [`MapBatcher`] — and then, instead of handing the batch to a
//! model, draws it:
//!
//! ```text
//! batches/batch_001.png    one row per crop: the picture, and the labels
//! ```
//!
//! Both panels come **back out of the tensors**, not out of the files the
//! batch was built from. That is the point: what is shown is what the network
//! is shown, so a channel swapped, a scale mistaken or a label off by a row
//! shows up here rather than as a run which will not learn.
//!
//! # Where the crop came from
//!
//! A fill pattern is drawn against the map's own ground and not against the
//! crop's, so a crop drawn at the origin comes out with its dots in the wrong
//! places even when every class in it is right. Each crop is therefore drawn
//! over the ground it was actually cut from, which [`MapCrop`] carries — and
//! the caption names the map and the corner, so a crop which looks wrong can
//! be found in `images/` and looked at whole.
//!
//! # With a run to compare against
//!
//! `--run <folder>` puts each crop through that run's network as well, adds a
//! panel for what it said, and prints a confusion matrix over every crop
//! drawn: what each class really was, against what the network called it.
//! That is the one table which says whether a network scoring well on pixels
//! is reading the map or has found the commonest answer — and whether it ever
//! calls a pixel the frame at all.
//!
//! Exit codes: 0 success, 1 usage error, 2 the batches could not be drawn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burn::data::dataloader::batcher::Batcher;
use burn::data::dataset::Dataset;
use burn::prelude::Backend;
use burn::tensor::{Int, Tensor};
use clap::Parser;
use image::{Rgb, RgbImage};

use maur_o::dataset::{Classes, CLASSES_FILE};
use maur_o::differences::compose;
use maur_o::geometry::Rect;
use maur_o::ground_truth::{BACKGROUND, NO_ROTATION};
use maur_o::net::data::{MapBatcher, MapCrop, MapDataset, CROP, DEFAULT_CROPS_PER_MAP};
use maur_o::net::predict::{load, resolve_angles, ReadBackSettings, ANGLE_LENGTH};
use maur_o::net::unet::UNet;
use maur_o::random::Random;
use maur_o::render::{render_map_over, to_rgb_image, Extent};
use maur_o::symbol_kinds::{Catalogue, Entry};
use maur_o::vectorize::{write_map, Placement, SymbolGrid};
use maur_o::xml_reader::read_xml_map;

/// Where the sheets go when no folder is named.
const DEFAULT_INTO: &str = "batches";

/// How many pixels a sheet leaves between one crop's row and the next.
///
/// The same as [`compose`] leaves between the panels of a row, so a sheet is
/// evenly spaced both ways.
const GAP: u32 = 8;

#[derive(Parser)]
#[command(
    name = "show_batches",
    version,
    about = "Draws the batches a training run would be shown, instead of training on them.\n\n\
             The crops are built exactly as `train` builds them -- the same split, crop size and \
             seed, through the same batcher -- and each is drawn twice, both panels read back \
             out of the tensors: the picture the network is handed, and the labels of that crop \
             vectorized and rendered over the very ground it was cut from.\n\n\
             With --run, a further panel shows what that run's network says about the same crop, \
             and a confusion matrix over every crop drawn is printed at the end."
)]
struct Args {
    /// The dataset folder: images/, gt/ and classes.json under it.
    dataset: PathBuf,

    /// Where the sheets are written. Made if it is not there.
    #[arg(default_value = DEFAULT_INTO)]
    into: PathBuf,

    /// How many batches to draw.
    #[arg(short = 'n', long, default_value_t = 2, value_name = "COUNT")]
    batches: usize,

    /// How many crops in one batch, which is how many rows a sheet has.
    #[arg(short = 'b', long, default_value_t = 4, value_name = "CROPS")]
    batch_size: usize,

    /// How many pixels square a crop is. Must divide by sixteen, as a run's
    /// must.
    #[arg(short = 'c', long, default_value_t = CROP, value_name = "PIXELS")]
    crop: usize,

    /// How many crops one pass takes from each map. With the seed, this is
    /// what decides where a crop lands, so it has to be the run's figure for
    /// these to be the run's crops.
    #[arg(long, default_value_t = DEFAULT_CROPS_PER_MAP, value_name = "COUNT")]
    crops_per_map: usize,

    /// How far a crop may hang off the edge of a map, in pixels, with what
    /// is past the edge filled in as white paper. Half the crop by default,
    /// which is a run's own default: these are meant to be the crops a run
    /// trains on, edges and all.
    #[arg(long, value_name = "PIXELS")]
    overhang: Option<usize>,

    /// The share of the maps kept for training; the rest validate against it.
    #[arg(long, default_value_t = 0.8, value_name = "SHARE")]
    train_share: f64,

    /// Which half to draw from. The validation half by default: it is the one
    /// a score is quoted over, so it is the one worth checking.
    #[arg(long, default_value = "valid", value_name = "HALF")]
    split: Half,

    /// What the crop positions and the pick of crops come out of. The same
    /// seed the run used gives the run's own crops.
    #[arg(short = 's', long, default_value_t = 0, value_name = "SEED")]
    seed: u64,

    /// A run folder under trainings/: adds a panel for what its network says
    /// about each crop, and prints a confusion matrix over all of them.
    #[arg(long, value_name = "FOLDER")]
    run: Option<PathBuf>,

    /// How many pixels of the crop's own edge to leave out of the confusion
    /// matrix. A convolution pads with nought and a U-Net of four levels does
    /// it nine times over, so a band around every crop is answered for partly
    /// out of padding rather than out of picture; leaving it out is what
    /// separates that from a class the network cannot tell apart anywhere.
    /// The pictures are drawn whole either way.
    #[arg(long, default_value_t = 0, value_name = "PIXELS")]
    margin: usize,

    /// The weights to use with --run. Defaults to best.mpk in the run folder;
    /// a checkpoint/007/model.mpk may be named instead.
    #[arg(short = 'W', long, value_name = "FILE")]
    weights: Option<PathBuf>,
}

/// Which half of the split to draw from.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Half {
    /// The maps a run trains on.
    Train,
    /// The maps it is held out from.
    Valid,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            e.print().ok();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };
    let usage = |message: String| (ExitCode::from(1), format!("Error: {message}"));
    let failed = |message: String| (ExitCode::from(2), format!("Error: {message}"));

    if !args.crop.is_multiple_of(16) {
        return Err(usage(format!(
            "Invalid value for --crop: {} does not divide by sixteen, which is what a U-Net of \
             four levels halves and doubles.",
            args.crop,
        )));
    }

    // What the dataset says about its own answers: which symbol a class is,
    // and how much ground a pixel covers. Drawing a label needs both, and
    // neither of them is in the label.
    let notes_file = args.dataset.join(CLASSES_FILE);
    let notes = Classes::read(&notes_file).map_err(&failed)?;
    let named = notes.symbol_set.clone().ok_or_else(|| {
        failed(format!(
            "{} names no symbol set, so there is no telling which symbol a class is",
            notes_file.display(),
        ))
    })?;
    let symbol_set = args.dataset.join(named);
    let (mut map, warnings) = read_xml_map(&symbol_set).map_err(|e| {
        failed(format!(
            "cannot read the symbol set {}: {e}",
            symbol_set.display()
        ))
    })?;
    map.resolve_references();
    for warning in &warnings {
        eprintln!("Warning: {warning}");
    }
    let symbols = Catalogue::of(&map).opaque_areas;
    let settings = ReadBackSettings::of(args.crop, notes.resolution, map.scale_denominator);

    // Which symbols turn, and how much of a turn each of them can actually
    // show -- the number the angle is folded by, and the one thing about the
    // target which is not visible in any of the panels. See
    // `maur_o::symbol_kinds::pattern_symmetry`.
    let turning: Vec<String> = symbols
        .iter()
        .filter(|symbol| symbol.turns)
        .map(|symbol| format!("{} every 1/{} turn", symbol.code, symbol.symmetry))
        .collect();
    println!(
        "{}: {}",
        symbol_set.display(),
        if turning.is_empty() {
            "no symbol here turns, so no crop carries an angle".to_string()
        } else {
            format!("the angles fold -- {}", turning.join(", "))
        },
    );

    let all = MapDataset::load(&args.dataset, args.seed)
        .and_then(|dataset| dataset.with_crop(args.crop))
        .map_err(&failed)?
        .with_crops_per_map(args.crops_per_map)
        .with_overhang(args.overhang.unwrap_or(args.crop / 2));
    let classes = all.classes();
    if classes != symbols.len() {
        return Err(failed(format!(
            "{} holds {} opaque areas and the labels were written for {classes}",
            symbol_set.display(),
            symbols.len(),
        )));
    }
    let (train, valid) = all.split(args.train_share);
    let half = match args.split {
        Half::Train => train,
        Half::Valid => valid,
    };
    println!(
        "{}: {} maps in the {} half, {} crops of {} pixels from each",
        args.dataset.display(),
        half.maps(),
        match args.split {
            Half::Train => "training",
            Half::Valid => "validation",
        },
        args.crops_per_map,
        args.crop,
    );

    // The model, where there is one to compare against. Loaded before the
    // first map is decoded: a run folder named wrongly should say so now
    // rather than after a minute of drawing.
    let device = device();
    let model = match &args.run {
        None => None,
        Some(run) => {
            let (model, config) =
                load::<Built>(run, args.weights.as_deref(), &device).map_err(&failed)?;
            if config.crop != args.crop {
                eprintln!(
                    "Warning: {} trained at {} pixels and these crops are {}: the network is \
                     being shown a size it never saw.",
                    run.display(),
                    config.crop,
                    args.crop,
                );
            }
            println!("{}: {BACKEND}, what it says in the last panel", run.display());
            Some(model)
        }
    };

    std::fs::create_dir_all(&args.into)
        .map_err(|e| failed(format!("cannot make {}: {e}", args.into.display())))?;
    // The maps go here while they are being drawn from, and no further: what
    // is kept of a batch is its sheet. Dropped at the end, which takes them
    // with it.
    let scratch = tempfile::tempdir()
        .map_err(|e| failed(format!("cannot make a folder for the drawn crops: {e}")))?;
    let scratch_map = scratch.path().join("crop.omap");

    // Which crops to take. The whole point is a sample of what a run is
    // shown, so they are picked from across the half rather than off the
    // front of it -- the first few maps of a folder were generated together
    // and are not a sample of anything.
    let mut random = Random::from_seed(args.seed);
    let wanted = args.batches * args.batch_size;
    let mut picked: Vec<usize> = (0..half.len()).collect();
    for at in (1..picked.len()).rev().take(wanted.min(picked.len())) {
        picked.swap(at, random.below(at + 1));
    }
    let picked: Vec<usize> = picked.into_iter().rev().take(wanted).collect();

    // What each class really was, against what the network called it, the
    // frame last in both. Left at nought where there is no network to
    // confuse.
    let mut confusion = vec![0u64; (classes + 1) * (classes + 1)];
    // How many pixels of each class were of each colour, and how many of
    // those the network got right -- which is what `print_patterns` divides
    // into the fill of a symbol and the pattern drawn over it.
    let mut coloured: HashMap<(usize, [u8; 3]), (u64, u64)> = HashMap::new();

    for (number, chunk) in picked.chunks(args.batch_size).enumerate() {
        let crops: Vec<MapCrop> = chunk
            .iter()
            .map(|&index| {
                half.get(index)
                    .unwrap_or_else(|| panic!("crop {index} is inside the dataset"))
            })
            .collect();

        // Through the batcher and straight back out of it. Round-tripping
        // rather than drawing the crops as they came keeps this a picture of
        // the batch and not of what went into the batch: the permutes and the
        // strides are where a label ends up on the wrong pixel, and they all
        // happen in here.
        let batch = MapBatcher.batch(crops.clone(), &device);
        let read = unbatch(&batch.image, &batch.class, &batch.angle);
        let said = model
            .as_ref()
            .map(|model| answers(model, batch.image.clone(), classes));

        let mut rows = Vec::with_capacity(read.len());
        for (index, crop) in read.iter().enumerate() {
            let from = &crops[index];
            let ground = ground_of(from, notes.image_size as usize, notes.resolution);
            let drawn = |grid| draw(grid, &ground, &symbol_set, &scratch_map, &settings);

            // The labels carry the folded angle the network is asked for, so
            // they need the same unfolding a prediction does before anything
            // draws them -- see `net::predict::resolve_angles`.
            let mut label_grid = labelled(crop, classes);
            resolve_angles(&mut label_grid, &symbols);
            let mut panels = vec![picture_of(crop), drawn(label_grid)?];
            let mut labels = vec![
                format!("{} at {},{}", from.map, from.left, from.top),
                format!("labels: {}", tally(&crop.class, classes, &symbols)),
            ];

            if let Some(said) = &said {
                let (class, turn) = &said[index];
                let shown = picture_of(crop);
                for (at, &was) in crop.class.iter().enumerate() {
                    if !inside(at, crop.size, args.margin) {
                        continue;
                    }
                    confusion[was as usize * (classes + 1) + class[at] as usize] += 1;
                    let colour = shown
                        .get_pixel((at % crop.size) as u32, (at / crop.size) as u32)
                        .0;
                    let counted = coloured.entry((was as usize, colour)).or_insert((0, 0));
                    counted.0 += 1;
                    counted.1 += u64::from(was == class[at]);
                }
                let right = crop
                    .class
                    .iter()
                    .zip(class)
                    .filter(|(&was, &said)| was == said)
                    .count();
                let mut grid = grid_of(class, turn, crop.size, classes);
                // Unfolded by the symbol's own symmetry, and taken off
                // entirely where the symbol has no pattern to turn: the same
                // two things `image_to_map` does to what a network says.
                resolve_angles(&mut grid, &symbols);
                panels.push(drawn(grid)?);
                labels.push(format!(
                    "said: {:.1}% of pixels right, {}",
                    100.0 * right as f64 / crop.class.len().max(1) as f64,
                    tally(class, classes, &symbols),
                ));
            }

            rows.push(compose(&panels, &labels));
        }

        let sheet = stack(&rows);
        let path = args.into.join(format!("batch_{:03}.png", number + 1));
        sheet
            .save(&path)
            .map_err(|e| failed(format!("cannot write {}: {e}", path.display())))?;
        println!("{}: {} crops", path.display(), rows.len());
    }

    if model.is_some() {
        print_confusion(&confusion, classes, &symbols, args.margin);
        print_patterns(&coloured, classes, &symbols);
    }
    Ok(())
}

/// The crops of a batch, back out of the tensors the network would be handed.
///
/// The names and the corners do not survive a batch — the batcher stacks
/// numbers — so what comes back here is the pixels alone, and the caller
/// pairs each with the [`MapCrop`] it put in.
fn unbatch<B: Backend>(
    image: &Tensor<B, 4>,
    class: &Tensor<B, 3, Int>,
    angle: &Tensor<B, 4>,
) -> Vec<MapCrop> {
    let [batch, _, size, _] = image.dims();
    let images: Vec<f32> = image.clone().into_data().iter::<f32>().collect();
    let classed: Vec<i32> = class.clone().into_data().iter::<i32>().collect();
    let angles: Vec<f32> = angle.clone().into_data().iter::<f32>().collect();

    let pixels = size * size;
    (0..batch)
        .map(|item| MapCrop {
            image: images[item * 3 * pixels..(item + 1) * 3 * pixels].to_vec(),
            class: classed[item * pixels..(item + 1) * pixels].to_vec(),
            angle: angles[item * 2 * pixels..(item + 1) * 2 * pixels].to_vec(),
            size,
            map: String::new(),
            left: 0,
            top: 0,
        })
        .collect()
}

/// What the network says about a batch: the class of every pixel, the frame
/// last, and the angle as a share of a whole turn.
///
/// One forward pass for the whole batch rather than a tile at a time — a crop
/// is already exactly the size the network takes, so there is nothing to
/// tile. The argmax and the threshold are `net::predict::predict_image`'s,
/// which is what makes this panel the same answer `image_to_map` would give.
fn answers<B: Backend>(
    model: &UNet<B>,
    image: Tensor<B, 4>,
    classes: usize,
) -> Vec<(Vec<i32>, Vec<f32>)> {
    let raw = model.forward(image);
    let [batch, channels, height, width] = raw.dims();
    let values: Vec<f32> = raw.into_data().iter::<f32>().collect();
    let pixels = height * width;

    (0..batch)
        .map(|item| {
            let of = |channel: usize, at: usize| values[(item * channels + channel) * pixels + at];
            let mut class = Vec::with_capacity(pixels);
            let mut turn = Vec::with_capacity(pixels);
            for at in 0..pixels {
                let winner = (0..=classes)
                    .max_by(|&a, &b| of(a, at).total_cmp(&of(b, at)))
                    .expect("a class or the frame");
                class.push(winner as i32);
                turn.push(angle_of(of(channels - 2, at), of(channels - 1, at)));
            }
            (class, turn)
        })
        .collect()
}

/// Whether a pixel is far enough from the crop's own edge to be counted.
///
/// Everything within `margin` of an edge was answered for partly out of the
/// nought a convolution pads with rather than out of the picture, and a
/// network is not being asked about the map there.
fn inside(at: usize, size: usize, margin: usize) -> bool {
    if margin == 0 {
        return true;
    }
    let (row, column) = (at / size, at % size);
    row >= margin && column >= margin && row + margin < size && column + margin < size
}

/// A share of a whole turn out of a sine and a cosine, and [`NO_ROTATION`]
/// where the pair is too short to be pointing anywhere.
fn angle_of(sin: f32, cos: f32) -> f32 {
    if sin.hypot(cos) <= ANGLE_LENGTH {
        // Not an angle got wrong: an angle declined, which is what the zero
        // vector says.
        NO_ROTATION
    } else {
        (sin.atan2(cos) / std::f32::consts::TAU).rem_euclid(1.0)
    }
}

/// The crop's own picture, back out of the `[-1, 1]` planes the network is
/// handed.
fn picture_of(crop: &MapCrop) -> RgbImage {
    let size = crop.size as u32;
    let pixels = crop.size * crop.size;
    let mut image = RgbImage::new(size.max(1), size.max(1));
    for (at, pixel) in image.pixels_mut().enumerate() {
        let channel = |c: usize| {
            ((crop.image[c * pixels + at] + 1.0) * 127.5).round().clamp(0.0, 255.0) as u8
        };
        *pixel = Rgb([channel(0), channel(1), channel(2)]);
    }
    image
}

/// The crop's labels as a grid the vectorizer takes: the angle back out of
/// its sine and cosine, and the frame back out of the last class.
fn labelled(crop: &MapCrop, classes: usize) -> SymbolGrid {
    let pixels = crop.size * crop.size;
    let turn: Vec<f32> = (0..pixels)
        .map(|at| angle_of(crop.angle[at], crop.angle[pixels + at]))
        .collect();
    grid_of(&crop.class, &turn, crop.size, classes)
}

/// A grid out of a class per pixel and a turn per pixel, the frame becoming
/// [`BACKGROUND`] — which is what [`write_map`] reads as nothing at all.
fn grid_of(class: &[i32], turn: &[f32], size: usize, classes: usize) -> SymbolGrid {
    let mut grid = SymbolGrid::new(size, size);
    grid.class = class
        .iter()
        .map(|&c| {
            if c < 0 || c as usize >= classes {
                BACKGROUND
            } else {
                c as u16
            }
        })
        .collect();
    grid.rotation = turn.to_vec();
    grid
}

/// The ground a crop covers, in meters, as the map it was cut from covers it.
///
/// A dataset image is centred on the origin — which is the square
/// `net::predict::read_back` places a whole picture over — so a crop is that
/// square shifted by its own corner. Getting this right is what makes the
/// drawn dots land where the picture's dots are: a pattern is drawn against
/// the map's ground and not against the crop's.
fn ground_of(crop: &MapCrop, image_size: usize, resolution: f64) -> Rect {
    let whole = image_size as f64 / resolution;
    let left = crop.left as f64 / resolution - whole / 2.0;
    let top = crop.top as f64 / resolution - whole / 2.0;
    let side = crop.size as f64 / resolution;
    Rect::from_ltrb(left, top, left + side, top + side)
}

/// A grid drawn: vectorized into a map, written, and rendered back out again.
///
/// The whole way round rather than a colour per class, and for the reason
/// `read_back` goes the whole way round: what is drawn is the map somebody
/// could open, so a class the vectorizer drops or a region it splits on an
/// angle shows up in the picture, instead of being smoothed over by a lookup
/// table which never went near the symbol set.
fn draw(
    grid: SymbolGrid,
    ground: &Rect,
    symbol_set: &Path,
    into: &Path,
    settings: &ReadBackSettings,
) -> Result<RgbImage, (ExitCode, String)> {
    let failed = |message: String| (ExitCode::from(2), format!("Error: {message}"));
    write_map(
        &grid,
        symbol_set,
        into,
        &Placement {
            ground: *ground,
            scale_denominator: settings.scale_denominator,
        },
        &settings.simplify,
    )
    .map_err(failed)?;
    let drawing = render_map_over(into, settings.resolution, Extent::Ground(*ground))
        .map_err(|e| failed(format!("cannot draw {}: {e}", into.display())))?;
    Ok(to_rgb_image(&drawing.pixmap))
}

/// How many pixels of each class a crop holds, the frame included, as a
/// caption says it.
fn tally(class: &[i32], classes: usize, symbols: &[Entry]) -> String {
    let mut counted = vec![0usize; classes + 1];
    for &c in class {
        counted[(c.max(0) as usize).min(classes)] += 1;
    }
    let parts: Vec<String> = counted
        .iter()
        .enumerate()
        .filter(|(_, &count)| count > 0)
        .map(|(at, &count)| {
            format!(
                "{} {:.0}%",
                name_of(at, classes, symbols),
                100.0 * count as f64 / class.len().max(1) as f64,
            )
        })
        .collect();
    if parts.is_empty() {
        "nothing".to_string()
    } else {
        parts.join(", ")
    }
}

/// What to call a class: the symbol's code, and "frame" for the one past
/// them.
fn name_of(at: usize, classes: usize, symbols: &[Entry]) -> String {
    if at >= classes {
        return "frame".to_string();
    }
    symbols
        .get(at)
        .map(|symbol| symbol.code.clone())
        .unwrap_or_else(|| at.to_string())
}

/// The rows of a sheet, one above the other.
fn stack(rows: &[RgbImage]) -> RgbImage {
    let width = rows.iter().map(|row| row.width()).max().unwrap_or(1);
    let height: u32 = rows.iter().map(|row| row.height()).sum::<u32>()
        + GAP * (rows.len() as u32).saturating_sub(1);
    let mut sheet = RgbImage::from_pixel(width.max(1), height.max(1), Rgb([255, 255, 255]));
    let mut y = 0;
    for row in rows {
        for (column, line, pixel) in row.enumerate_pixels() {
            sheet.put_pixel(column, y + line, *pixel);
        }
        y += row.height() + GAP;
    }
    sheet
}

/// What each class really was, against what the network called it.
fn print_confusion(confusion: &[u64], classes: usize, symbols: &[Entry], margin: usize) {
    let width = (0..=classes)
        .map(|at| name_of(at, classes, symbols).len())
        .max()
        .unwrap_or(5)
        .max(5);

    println!("\nwhat each class really was, row by row, against what the network called it:");
    if margin > 0 {
        println!("(the outer {margin} pixels of every crop left out)");
    }
    print!("{:>width$} |", "", width = width);
    for at in 0..=classes {
        print!(" {:>7}", name_of(at, classes, symbols));
    }
    println!("  |     pixels");

    let (mut right, mut total) = (0u64, 0u64);
    for was in 0..=classes {
        let row = &confusion[was * (classes + 1)..(was + 1) * (classes + 1)];
        let held: u64 = row.iter().sum();
        print!(
            "{:>width$} |",
            name_of(was, classes, symbols),
            width = width
        );
        for said in row.iter().take(classes + 1) {
            print!(" {:>6.1}%", 100.0 * *said as f64 / held.max(1) as f64);
        }
        println!("  | {held:>10}");
        right += row[was];
        total += held;
    }
    println!(
        "\n{:.2}% of {total} pixels were called right.",
        100.0 * right as f64 / total.max(1) as f64,
    );
}

/// How a class is read where its fill is plain, against where its pattern is
/// drawn over it.
///
/// A patterned area is one colour nearly everywhere and another on the dots
/// or hatching, and the two are not the same question. On the ink, the pixel
/// says what it is by its own colour and the network has only to know which
/// symbol draws that ink; between the dots, the colour is the fill and
/// nothing but the pattern *around* the pixel says which symbol it belongs
/// to — which the first convolution cannot see, and only a level far enough
/// down to hold two dots at once can.
///
/// So this is where the answer to "can it tell two symbols apart when the
/// window is not on a dot" is: the fill column. A class read well on its ink
/// and badly on its fill is a network reading colours rather than patterns.
///
/// The fill is taken to be the commonest colour the class was drawn in and
/// the ink everything else, which is what a pattern over a flat area comes
/// to; a symbol with no pattern has an ink of nothing but its own
/// antialiased edges.
fn print_patterns(
    coloured: &HashMap<(usize, [u8; 3]), (u64, u64)>,
    classes: usize,
    symbols: &[Entry],
) {
    println!("\nand how it read each class on its own fill, against on the pattern over it:");
    println!(
        "{:>7} | {:>10} {:>8} | {:>10} {:>8} | fill colour",
        "", "fill px", "right", "ink px", "right",
    );
    for class in 0..=classes {
        let of_class: Vec<(&[u8; 3], &(u64, u64))> = coloured
            .iter()
            .filter(|((held, _), _)| *held == class)
            .map(|((_, colour), counted)| (colour, counted))
            .collect();
        let Some((fill, filled)) = of_class
            .iter()
            .max_by_key(|(_, counted)| counted.0)
            .map(|(colour, counted)| (**colour, **counted))
        else {
            continue;
        };
        let ink = of_class
            .iter()
            .filter(|(colour, _)| **colour != fill)
            .fold((0u64, 0u64), |(total, right), (_, counted)| {
                (total + counted.0, right + counted.1)
            });
        let share = |(total, right): (u64, u64)| 100.0 * right as f64 / total.max(1) as f64;
        println!(
            "{:>7} | {:>10} {:>7.1}% | {:>10} {:>7.1}% | {},{},{}",
            name_of(class, classes, symbols),
            filled.0,
            share(filled),
            ink.0,
            share(ink),
            fill[0],
            fill[1],
            fill[2],
        );
    }
}

// Which backend was built in -- see `train`, whose three these are. No
// gradient tape over any of them: nothing here trains.
#[cfg(feature = "cuda")]
mod backend {
    /// What the tool prints for the backend it was built against.
    pub const BACKEND: &str = "CUDA";
    /// NVIDIA, through CUDA.
    pub type Backend = burn::backend::Cuda;
    /// The one device, whichever it is.
    pub fn device() -> burn::backend::cuda::CudaDevice {
        Default::default()
    }
}

#[cfg(all(feature = "wgpu", not(feature = "cuda")))]
mod backend {
    /// What the tool prints for the backend it was built against.
    pub const BACKEND: &str = "wgpu";
    /// Any GPU with a Vulkan, Metal or DX12 driver.
    pub type Backend = burn::backend::Wgpu;
    /// The one device, whichever it is.
    pub fn device() -> burn::backend::wgpu::WgpuDevice {
        Default::default()
    }
}

#[cfg(not(any(feature = "cuda", feature = "wgpu")))]
mod backend {
    /// What the tool prints for the backend it was built against.
    pub const BACKEND: &str = "ndarray";
    /// The pure Rust backend, which runs anywhere the renderer does.
    pub type Backend = burn::backend::NdArray;
    /// The one device, whichever it is.
    pub fn device() -> burn::backend::ndarray::NdArrayDevice {
        Default::default()
    }
}

// The trait of the same name is already in scope, and generic code here is
// written against it: the concrete one needs another name to be named by.
use backend::{device, Backend as Built, BACKEND};
