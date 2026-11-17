//! `mauro net train`'s screen: a dataset, and how a network is trained on it. It runs detached, and the home screen's job panel is its dashboard, so `--dashboard` is not offered.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const DATASET: usize = 0;
const TRAININGS: usize = 1;
const EPOCHS: usize = 2;
const BATCH_SIZE: usize = 3;
const CROP: usize = 4;
const CROPS_PER_MAP: usize = 5;
const BASE_CHANNELS: usize = 6;
const LEARNING_RATE: usize = 7;
const ANGLE_WEIGHT: usize = 8;
const WEIGHT_DECAY: usize = 9;
const OVERHANG: usize = 10;
const TRAIN_SHARE: usize = 11;
const WORKERS: usize = 12;
const IMAGE_VALID: usize = 13;
const SEED: usize = 14;

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
                Field::text("Epochs").arg("epochs"),
                Field::text("Batch size").arg("batch_size").unit("crops"),
                Field::text("Crop").arg("crop").unit("px"),
                Field::text("Crops per map").arg("crops_per_map"),
                Field::text("Base channels").arg("base_channels"),
                Field::text("Learning rate").arg("learning_rate"),
                Field::text("Angle weight").arg("angle_weight"),
                Field::text("Weight decay").arg("weight_decay"),
                Field::text("Overhang").arg("overhang").unit("px"),
                Field::text("Train share").arg("train_share"),
                Field::text("Workers").arg("workers"),
                Field::text("Image valid").arg("image_valid"),
                Field::text("Seed").arg("seed"),
            ],
        );
        form.open_picker(DATASET);
        Self { form }
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
        form.push_option(&mut args, EPOCHS, "--epochs");
        form.push_option(&mut args, BATCH_SIZE, "--batch-size");
        form.push_option(&mut args, CROP, "--crop");
        form.push_option(&mut args, CROPS_PER_MAP, "--crops-per-map");
        form.push_option(&mut args, BASE_CHANNELS, "--base-channels");
        form.push_option(&mut args, LEARNING_RATE, "--learning-rate");
        form.push_option(&mut args, ANGLE_WEIGHT, "--angle-weight");
        form.push_option(&mut args, WEIGHT_DECAY, "--weight-decay");
        form.push_option(&mut args, OVERHANG, "--overhang");
        form.push_option(&mut args, TRAIN_SHARE, "--train-share");
        form.push_option(&mut args, WORKERS, "--workers");
        form.push_option(&mut args, IMAGE_VALID, "--image-valid");
        form.push_option(&mut args, SEED, "--seed");
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
}
