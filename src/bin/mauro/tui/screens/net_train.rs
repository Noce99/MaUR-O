//! `mauro net train`'s screen: a dataset, and how a network is trained on it. It runs detached, and the home screen's job panel is its dashboard, so `--dashboard` is not offered.

use ratatui::style::Stylize;
use ratatui::text::Line;

use maur_o::dataset::{Classes, CLASSES_FILE};
use maur_o::net::unet::UNetConfig;

use super::pair;
use crate::tui::form::{Field, Form, Tool, CHOICE_NOTE, ERROR_COLOR};
use crate::tui::gpu::{Probe, Snapshot};
use crate::tui::widgets::Pick;

const DATASET: usize = 0;
const TRAININGS: usize = 1;
const GPU: usize = 2;
const MODEL: usize = 3;
const EPOCHS: usize = 4;
const BATCH_SIZE: usize = 5;
const CROP: usize = 6;
const CROPS_PER_MAP: usize = 7;
const BASE_CHANNELS: usize = 8;
const LEARNING_RATE: usize = 9;
const ANGLE_WEIGHT: usize = 10;
const LINE_WEIGHT: usize = 11;
const POINT_WEIGHT: usize = 12;
const SCALE_WEIGHT: usize = 13;
const WEIGHT_DECAY: usize = 14;
const PATIENCE: usize = 15;
const OVERHANG: usize = 16;
const TRAIN_SHARE: usize = 17;
const WORKERS: usize = 18;
const IMAGE_VALID: usize = 19;
const SEED: usize = 20;
const SIZE: usize = 21;
const DEPTH: usize = 22;
const CONTROL_POINTS: usize = 23;
const SAMPLES: usize = 24;
const TEMPERATURE_START: usize = 25;
const TEMPERATURE_END: usize = 26;
const FROM: usize = 27;

/// The fields only the U-Net has, and the ones only the tree has.
const UNET_ONLY: [usize; 12] = [
    CROP,
    CROPS_PER_MAP,
    ANGLE_WEIGHT,
    LINE_WEIGHT,
    POINT_WEIGHT,
    SCALE_WEIGHT,
    WEIGHT_DECAY,
    PATIENCE,
    OVERHANG,
    WORKERS,
    IMAGE_VALID,
    FROM,
];
const TREE_ONLY: [usize; 6] = [
    SIZE,
    DEPTH,
    CONTROL_POINTS,
    SAMPLES,
    TEMPERATURE_START,
    TEMPERATURE_END,
];

/// What a training process holds on a GPU before it holds a tensor: the
/// driver's context and the backend's own, in bytes.
const PROCESS_OVERHEAD: u64 = 600 << 20;

/// How many classes a dataset not read yet is taken to have, for the memory
/// a run of it would want.
const GUESSED_CLASSES: usize = 20;

/// What a GPU would be asked for by a run, and what it has.
#[derive(Clone, Debug, PartialEq)]
struct Fit {
    gpu: String,
    needed: u64,
    free: u64,
    total: u64,
}

impl Fit {
    fn fits(&self) -> bool {
        self.needed <= self.free
    }
}

/// Bytes, as GiB.
fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

pub struct NetTrain {
    form: Form,
    /// Whether this build trains on a GPU: on the CPU there is none to pick.
    on_gpu: bool,
    probe: Probe,
    /// What the probe last said.
    gpus: Snapshot,
    /// What the dataset picked says of itself, where it says it.
    classes: Option<Classes>,
}

impl NetTrain {
    pub fn new() -> Self {
        Self::with(Probe::start(), crate::net_train::ON_GPU)
    }

    /// The screen, the GPUs asked of `probe`; `on_gpu` for a build which
    /// trains on one.
    fn with(probe: Probe, on_gpu: bool) -> Self {
        let mut form = Form::new(
            &["net", "train"],
            "Train",
            vec![
                Field::path("Dataset", Pick::Folder).arg("dataset"),
                Field::path("Trainings", Pick::Folder).arg("trainings"),
                Field::choice("GPU", &["0"]).arg("gpu"),
                Field::choice("Model", &["unet", "tree"]).arg("model").help(
                    "unet reads a class for every pixel, on crops of the maps; tree answers \
                     with the curves which cut the picture into areas and a symbol for each, \
                     on whole pictures reduced to Size.",
                ),
                Field::text("Epochs").arg("epochs"),
                Field::text("Batch size").arg("batch_size").unit("crops"),
                Field::text("Crop").arg("crop").unit("px"),
                Field::text("Crops per map").arg("crops_per_map"),
                Field::text("Base channels").arg("base_channels"),
                Field::text("Learning rate").arg("learning_rate"),
                Field::text("Angle weight").arg("angle_weight"),
                Field::text("Line weight").arg("line_weight"),
                Field::text("Point weight").arg("point_weight"),
                Field::text("Scale weight").arg("scale_weight"),
                Field::text("Weight decay").arg("weight_decay"),
                Field::text("Patience").arg("patience").unit("epochs"),
                Field::text("Overhang").arg("overhang").unit("px"),
                Field::text("Train share").arg("train_share"),
                Field::text("Workers").arg("workers"),
                Field::text("Image valid").arg("image_valid"),
                Field::text("Seed").arg("seed"),
                Field::text("Size").arg("size").unit("px"),
                Field::text("Depth").arg("depth").unit("cuts"),
                Field::text("Control points").arg("control_points"),
                Field::text("Samples").arg("samples"),
                Field::text("Temperature start").arg("temperature_start"),
                Field::text("Temperature end").arg("temperature_end"),
                Field::path("From", Pick::FileOrFolder(&["bpk"])).arg("from"),
            ],
        );
        // Until the driver answers, the one the default names.
        let default_gpu = form.default(GPU).unwrap_or("0").to_string();
        form.set_choices(GPU, vec![default_gpu]);
        form.show(GPU, on_gpu);
        form.open_picker(DATASET);
        let mut screen = Self {
            form,
            on_gpu,
            probe,
            gpus: Snapshot::default(),
            classes: None,
        };
        screen.update();
        screen.read_classes();
        screen.tick();
        screen
    }

    fn tree(&self) -> bool {
        self.form.choice_value(MODEL) == "tree"
    }

    /// Shows the options of the model picked and hides the other's.
    fn update(&mut self) {
        let tree = self.tree();
        for field in UNET_ONLY {
            self.form.show(field, !tree);
        }
        for field in TREE_ONLY {
            self.form.show(field, tree);
        }
    }

    /// Reads what the dataset picked says of itself, and marks it red where
    /// it is not one a run can be trained on.
    fn read_classes(&mut self) {
        let dir = self.form.given(DATASET).map(std::path::PathBuf::from);
        let read = dir
            .as_ref()
            .map(|dir| Classes::read(&dir.join(CLASSES_FILE)));
        let problem = match (&dir, &read) {
            (Some(dir), Some(Err(_))) if !dir.is_dir() => {
                Some("There is no such folder.".to_string())
            }
            (Some(_), Some(Err(_))) => Some(format!(
                "No {CLASSES_FILE} in it: not a labelled dataset. Generate one with Content \
                 areas or areas-and-lines; all draws what nothing could label."
            )),
            _ => None,
        };
        self.classes = read.and_then(Result::ok);
        self.form.set_problem(DATASET, problem);
    }

    /// The GPUs the driver lists, as choices: `1 · NVIDIA GeForce RTX 4060 Ti`.
    fn gpu_choices(&self) -> Vec<String> {
        self.gpus
            .gpus
            .iter()
            .map(|g| format!("{}{CHOICE_NOTE}{}", g.index, g.name))
            .collect()
    }

    /// What a U-Net run as the fields say would want of the GPU picked, and
    /// what that has; `None` where it cannot be said (the tree, a field not a
    /// number yet, no driver to ask).
    fn fit(&self) -> Option<Fit> {
        if self.tree() || !self.on_gpu {
            return None;
        }
        let form = &self.form;
        let batch: usize = form.text(BATCH_SIZE).parse().ok()?;
        let crop: usize = form.text(CROP).parse().ok()?;
        let base: usize = form.text(BASE_CHANNELS).parse().ok()?;
        let index: u32 = form.choice_value(GPU).parse().ok()?;
        let gpu = self.gpus.gpus.iter().find(|g| g.index == index)?;
        let total = gpu.memory_total? << 20;
        let free = total.saturating_sub(gpu.memory_used.unwrap_or(0) << 20);
        let (classes, lines, points, scale) = match &self.classes {
            Some(c) => (c.classes, c.lines, c.points, c.resolution_range.is_some()),
            None => (GUESSED_CLASSES, 0, 0, false),
        };
        let config = UNetConfig::new(classes)
            .with_base_channels(base)
            .with_lines(lines)
            .with_points(points)
            .with_scale_head(scale);
        let needed = config.training_bytes(batch, crop)? + PROCESS_OVERHEAD;
        Some(Fit {
            gpu: format!("GPU {index}"),
            needed,
            free,
            total,
        })
    }

    /// Marks the batch size red where it would not fit.
    fn check_fit(&mut self) {
        let problem = self.fit().filter(|f| !f.fits()).map(|f| {
            format!(
                "A step of this batch wants about {} of {}, which has {} free: try a smaller \
                 batch, crop or base.",
                gib(f.needed),
                f.gpu,
                gib(f.free)
            )
        });
        self.form.set_problem(BATCH_SIZE, problem);
    }
}

impl Tool for NetTrain {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::net_train::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        match field {
            MODEL => self.update(),
            DATASET => self.read_classes(),
            _ => {}
        }
        self.check_fit();
    }

    fn tick(&mut self) {
        let snapshot = self.probe.snapshot();
        if snapshot != self.gpus {
            self.gpus = snapshot;
            if self.gpus.available && !self.gpus.gpus.is_empty() {
                let choices = self.gpu_choices();
                self.form.set_choices(GPU, choices);
            }
            self.check_fit();
        }
    }

    fn background(&self) -> Option<&'static str> {
        Some(crate::tui::jobs::TRAINING)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        if !self.on_gpu {
            return None;
        }
        let mut lines = Vec::new();
        if !self.gpus.available {
            lines.push(Line::from("No nvidia-smi to ask about the GPUs.").dim());
            return Some(("GPU", lines));
        }
        match self.fit() {
            Some(fit) => {
                let need = pair("Batch", format!("≈ {} a step", gib(fit.needed)));
                lines.push(if fit.fits() {
                    need.green()
                } else {
                    need.fg(ERROR_COLOR)
                });
                lines.push(pair(
                    "Free",
                    format!("{} of {} on {}", gib(fit.free), gib(fit.total), fit.gpu),
                ));
                if self.classes.is_none() {
                    lines.push(
                        Line::from(format!(
                            "(guessing {GUESSED_CLASSES} classes: no dataset read yet)"
                        ))
                        .dim(),
                    );
                }
            }
            None if self.tree() => {
                lines.push(Line::from("The tree is small: no estimate for it.").dim())
            }
            None => lines.push(Line::from("(the sizes are not numbers yet)").dim()),
        }
        Some(("GPU", lines))
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["net".to_string(), "train".to_string()];
        args.push(
            form.given(DATASET)
                .ok_or("Pick the dataset folder (Enter on Dataset).")?
                .to_string(),
        );
        if let Some(trainings) = form
            .given(TRAININGS)
            .filter(|_| !form.is_default(TRAININGS))
        {
            args.push(trainings.to_string());
        }
        for (field, flag) in [(GPU, "--gpu"), (MODEL, "--model")] {
            let value = form.choice_value(field);
            if form.shown(field) && Some(value) != form.default(field) {
                args.extend([flag.to_string(), value.to_string()]);
            }
        }
        form.push_option(&mut args, EPOCHS, "--epochs");
        form.push_option(&mut args, BATCH_SIZE, "--batch-size");
        form.push_option(&mut args, CROP, "--crop");
        form.push_option(&mut args, CROPS_PER_MAP, "--crops-per-map");
        form.push_option(&mut args, BASE_CHANNELS, "--base-channels");
        form.push_option(&mut args, LEARNING_RATE, "--learning-rate");
        form.push_option(&mut args, ANGLE_WEIGHT, "--angle-weight");
        form.push_option(&mut args, LINE_WEIGHT, "--line-weight");
        form.push_option(&mut args, POINT_WEIGHT, "--point-weight");
        form.push_option(&mut args, SCALE_WEIGHT, "--scale-weight");
        form.push_option(&mut args, WEIGHT_DECAY, "--weight-decay");
        form.push_option(&mut args, PATIENCE, "--patience");
        form.push_option(&mut args, OVERHANG, "--overhang");
        form.push_option(&mut args, TRAIN_SHARE, "--train-share");
        form.push_option(&mut args, WORKERS, "--workers");
        form.push_option(&mut args, IMAGE_VALID, "--image-valid");
        form.push_option(&mut args, SEED, "--seed");
        form.push_option(&mut args, SIZE, "--size");
        form.push_option(&mut args, DEPTH, "--depth");
        form.push_option(&mut args, CONTROL_POINTS, "--control-points");
        form.push_option(&mut args, SAMPLES, "--samples");
        form.push_option(&mut args, TEMPERATURE_START, "--temperature-start");
        form.push_option(&mut args, TEMPERATURE_END, "--temperature-end");
        if let Some(from) = form.given(FROM) {
            args.extend(["--from".to_string(), from.to_string()]);
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::tests::{press, type_into};
    use crate::tui::form::{command, State};
    use crate::tui::gpu::{parse_gpus, Snapshot};
    use ratatui::crossterm::event::KeyCode;

    /// A small GPU and a large one, the large one with a little in use.
    fn two_gpus() -> Snapshot {
        Snapshot {
            available: true,
            gpus: parse_gpus(
                "0, GPU-aaa, NVIDIA GeForce GTX 1650, 4, 4096, 0, 36, 6.38, 85.00, 300\n\
                 1, GPU-bbb, NVIDIA GeForce RTX 4060 Ti, 383, 16380, 0, 40, 10, 165, 2310\n",
            ),
            apps: Vec::new(),
        }
    }

    fn screen() -> NetTrain {
        let mut screen = NetTrain::with(Probe::fixed(two_gpus()), true);
        type_into(&mut screen.form, DATASET, "data");
        screen
    }

    #[test]
    fn the_required_arguments_make_a_command() {
        let mut screen = NetTrain::with(Probe::fixed(Snapshot::default()), false);
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, DATASET, "data");
        assert_eq!(command(&screen).unwrap(), ["net", "train", "data"]);
        type_into(&mut screen.form, EPOCHS, "3");
        assert_eq!(command(&screen).unwrap()[3..], ["--epochs", "3"]);
        assert!(!screen.form.shown(GPU), "no GPU to pick on the CPU");
    }

    #[test]
    fn a_folder_with_no_classes_is_red() {
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen();
        type_into(&mut screen.form, DATASET, &dir.path().display().to_string());
        screen.changed(DATASET);
        let State::Error(why) = screen.form.state(DATASET) else {
            panic!("{:?}", screen.form.state(DATASET));
        };
        assert!(why.contains("classes.json"), "{why}");
        type_into(&mut screen.form, DATASET, "no/such/folder");
        screen.changed(DATASET);
        assert!(matches!(screen.form.state(DATASET), State::Error(w) if w.contains("no such")));
    }

    #[test]
    fn the_gpus_are_the_drivers_and_the_first_is_the_default() {
        let mut screen = screen();
        assert_eq!(screen.form.choice(GPU), "0 · NVIDIA GeForce GTX 1650");
        assert_eq!(command(&screen).unwrap(), ["net", "train", "data"]);
        press(&mut screen.form, GPU, KeyCode::Right);
        screen.changed(GPU);
        assert_eq!(screen.form.choice_value(GPU), "1");
        assert_eq!(screen.form.state(GPU), State::Changed);
        assert_eq!(command(&screen).unwrap()[3..], ["--gpu", "1"]);
    }

    #[test]
    fn a_batch_too_large_for_the_gpu_is_red() {
        let mut screen = screen();
        assert!(screen.fit().unwrap().fits(), "the default fits in 4 GiB");
        assert_eq!(screen.form.state(BATCH_SIZE), State::Default);
        type_into(&mut screen.form, BATCH_SIZE, "64");
        screen.changed(BATCH_SIZE);
        assert!(!screen.fit().unwrap().fits());
        assert!(matches!(screen.form.state(BATCH_SIZE), State::Error(_)));
        // The same batch on the larger GPU fits again.
        press(&mut screen.form, GPU, KeyCode::Right);
        screen.changed(GPU);
        type_into(&mut screen.form, BATCH_SIZE, "16");
        screen.changed(BATCH_SIZE);
        assert_eq!(screen.form.state(BATCH_SIZE), State::Changed);
        let (_, lines) = screen.panel().unwrap();
        assert!(lines[0].to_string().contains("GiB a step"), "{lines:?}");
    }

    #[test]
    fn the_tree_has_its_own_options_and_the_unet_loses_its_own() {
        let mut screen = screen();
        assert!(!screen.form.shown(DEPTH));
        assert!(screen.form.shown(CROP));
        press(&mut screen.form, MODEL, KeyCode::Right);
        screen.changed(MODEL);
        assert_eq!(screen.form.choice(MODEL), "tree");
        assert!(screen.form.shown(DEPTH) && !screen.form.shown(CROP));
        assert_eq!(screen.fit(), None, "no estimate for the tree");
        type_into(&mut screen.form, DEPTH, "5");
        type_into(&mut screen.form, CROP, "32");
        assert_eq!(
            command(&screen).unwrap(),
            ["net", "train", "data", "--model", "tree", "--depth", "5"]
        );
    }

    /// A past run to start from is passed as --from, and only for the
    /// U-Net: the tree has no such option.
    #[test]
    fn a_past_run_to_start_from_is_the_unets_alone() {
        let mut screen = screen();
        assert!(screen.form.shown(FROM));
        type_into(&mut screen.form, FROM, "trainings/old");
        assert_eq!(
            command(&screen).unwrap(),
            ["net", "train", "data", "--from", "trainings/old"]
        );
        press(&mut screen.form, MODEL, KeyCode::Right);
        screen.changed(MODEL);
        assert!(!screen.form.shown(FROM));
        assert!(!command(&screen).unwrap().contains(&"--from".to_string()));
    }
}
