//! `mauro net show-batches`'s screen: the batches a run would be trained on, drawn from the tensors.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const DATASET: usize = 0;
const INTO: usize = 1;
const BATCHES: usize = 2;
const BATCH_SIZE: usize = 3;
const CROP: usize = 4;
const CROPS_PER_MAP: usize = 5;
const OVERHANG: usize = 6;
const TRAIN_SHARE: usize = 7;
const SPLIT: usize = 8;
const SEED: usize = 9;
const RUN: usize = 10;
const MARGIN: usize = 11;
const WEIGHTS: usize = 12;

pub struct NetShowBatches {
    form: Form,
}

impl NetShowBatches {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["net", "show-batches"],
            "Draw",
            vec![
                Field::path("Dataset", Pick::Folder).arg("dataset"),
                Field::path("Into", Pick::Folder).arg("into"),
                Field::text("Batches").arg("batches"),
                Field::text("Batch size").arg("batch_size").unit("crops"),
                Field::text("Crop").arg("crop").unit("px"),
                Field::text("Crops per map").arg("crops_per_map"),
                Field::text("Overhang").arg("overhang").unit("px"),
                Field::text("Train share").arg("train_share"),
                Field::choice("Split", &["valid", "train"]).arg("split"),
                Field::text("Seed").arg("seed"),
                Field::path("Run", Pick::Folder).arg("run"),
                Field::text("Margin").arg("margin").unit("px"),
                Field::path("Weights", Pick::AnyFile).arg("weights"),
            ],
        );
        form.open_picker(DATASET);
        Self { form }
    }
}

impl Tool for NetShowBatches {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::net_show_batches::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["net".to_string(), "show-batches".to_string()];
        args.push(
            form.given(DATASET)
                .ok_or("Pick the dataset folder (Enter on Dataset).")?
                .to_string(),
        );
        if let Some(into) = form.given(INTO).filter(|_| !form.is_default(INTO)) {
            args.push(into.to_string());
        }
        form.push_option(&mut args, BATCHES, "--batches");
        form.push_option(&mut args, BATCH_SIZE, "--batch-size");
        form.push_option(&mut args, CROP, "--crop");
        form.push_option(&mut args, CROPS_PER_MAP, "--crops-per-map");
        form.push_option(&mut args, OVERHANG, "--overhang");
        form.push_option(&mut args, TRAIN_SHARE, "--train-share");
        if form.choice(SPLIT) != "valid" {
            args.extend(["--split".to_string(), form.choice(SPLIT).to_string()]);
        }
        form.push_option(&mut args, SEED, "--seed");
        form.push_option(&mut args, RUN, "--run");
        form.push_option(&mut args, MARGIN, "--margin");
        form.push_option(&mut args, WEIGHTS, "--weights");
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
        let mut screen = NetShowBatches::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, DATASET, "data");
        assert_eq!(command(&screen).unwrap(), ["net", "show-batches", "data"]);
        type_into(&mut screen.form, BATCHES, "5");
        assert_eq!(command(&screen).unwrap()[3..], ["--batches", "5"]);
    }
}
