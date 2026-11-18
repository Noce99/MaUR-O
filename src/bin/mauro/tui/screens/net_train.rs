//! `mauro net train`'s screen: a dataset, and how a network is trained on it. It runs detached, and the home screen's job panel is its dashboard, so `--dashboard` is not offered.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const DATASET: usize = 0;
const TRAININGS: usize = 1;
const MODEL: usize = 2;
const EPOCHS: usize = 3;
const BATCH_SIZE: usize = 4;
const CROP: usize = 5;
const CROPS_PER_MAP: usize = 6;
const BASE_CHANNELS: usize = 7;
const LEARNING_RATE: usize = 8;
const ANGLE_WEIGHT: usize = 9;
const LINE_WEIGHT: usize = 10;
const SCALE_WEIGHT: usize = 11;
const WEIGHT_DECAY: usize = 12;
const OVERHANG: usize = 13;
const TRAIN_SHARE: usize = 14;
const WORKERS: usize = 15;
const IMAGE_VALID: usize = 16;
const SEED: usize = 17;
const SIZE: usize = 18;
const DEPTH: usize = 19;
const CONTROL_POINTS: usize = 20;
const SAMPLES: usize = 21;
const TEMPERATURE_START: usize = 22;
const TEMPERATURE_END: usize = 23;

/// The fields only the U-Net has, and the ones only the tree has.
const UNET_ONLY: [usize; 9] = [
    CROP,
    CROPS_PER_MAP,
    ANGLE_WEIGHT,
    LINE_WEIGHT,
    SCALE_WEIGHT,
    WEIGHT_DECAY,
    OVERHANG,
    WORKERS,
    IMAGE_VALID,
];
const TREE_ONLY: [usize; 6] = [
    SIZE,
    DEPTH,
    CONTROL_POINTS,
    SAMPLES,
    TEMPERATURE_START,
    TEMPERATURE_END,
];

pub struct NetTrain {
    form: Form,
}

impl NetTrain {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["net", "train"],
            "Train",
            vec![
                Field::path("Dataset", Pick::Folder).arg("dataset"),
                Field::path("Trainings", Pick::Folder).arg("trainings"),
                Field::choice("Model", &["unet", "tree"]).help(
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
                Field::text("Scale weight").arg("scale_weight"),
                Field::text("Weight decay").arg("weight_decay"),
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
            ],
        );
        form.open_picker(DATASET);
        let mut screen = Self { form };
        screen.update();
        screen
    }

    fn tree(&self) -> bool {
        self.form.choice(MODEL) == "tree"
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
        if field == MODEL {
            self.update();
        }
    }

    fn background(&self) -> Option<&'static str> {
        Some(crate::tui::jobs::TRAINING)
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
        if self.tree() {
            args.extend(["--model".to_string(), "tree".to_string()]);
        }
        form.push_option(&mut args, EPOCHS, "--epochs");
        form.push_option(&mut args, BATCH_SIZE, "--batch-size");
        form.push_option(&mut args, CROP, "--crop");
        form.push_option(&mut args, CROPS_PER_MAP, "--crops-per-map");
        form.push_option(&mut args, BASE_CHANNELS, "--base-channels");
        form.push_option(&mut args, LEARNING_RATE, "--learning-rate");
        form.push_option(&mut args, ANGLE_WEIGHT, "--angle-weight");
        form.push_option(&mut args, LINE_WEIGHT, "--line-weight");
        form.push_option(&mut args, SCALE_WEIGHT, "--scale-weight");
        form.push_option(&mut args, WEIGHT_DECAY, "--weight-decay");
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
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::type_into;

    #[test]
    fn the_required_arguments_make_a_command() {
        let mut screen = NetTrain::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, DATASET, "data");
        assert_eq!(command(&screen).unwrap(), ["net", "train", "data"]);
        type_into(&mut screen.form, EPOCHS, "3");
        assert_eq!(command(&screen).unwrap()[3..], ["--epochs", "3"]);
    }

    #[test]
    fn the_tree_has_its_own_options_and_the_unet_loses_its_own() {
        let mut screen = NetTrain::new();
        type_into(&mut screen.form, DATASET, "data");
        assert!(!screen.form.shown(DEPTH));
        assert!(screen.form.shown(CROP));
        crate::tui::form::tests::press(
            &mut screen.form,
            MODEL,
            ratatui::crossterm::event::KeyCode::Right,
        );
        screen.changed(MODEL);
        assert_eq!(screen.form.choice(MODEL), "tree");
        assert!(screen.form.shown(DEPTH) && !screen.form.shown(CROP));
        type_into(&mut screen.form, DEPTH, "5");
        type_into(&mut screen.form, CROP, "32");
        assert_eq!(
            command(&screen).unwrap(),
            ["net", "train", "data", "--model", "tree", "--depth", "5"]
        );
    }
}
