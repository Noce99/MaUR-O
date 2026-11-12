//! `mauro dem-eval`'s screen: two GeoTIFFs and where to put the scores.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const TIFF: &[&str] = &["tif", "tiff"];

const GROUND_TRUTH: usize = 0;
const PREDICTION: usize = 1;
const RESULTS: usize = 2;

pub struct DemEval {
    form: Form,
}

impl DemEval {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["dem-eval"],
            "Score",
            vec![
                Field::path("Ground truth", Pick::Files(TIFF)).arg("ground_truth"),
                Field::path("Prediction", Pick::Files(TIFF)).arg("prediction"),
                Field::path("Results", Pick::Folder).arg("results"),
            ],
        );
        form.open_picker(GROUND_TRUTH);
        Self { form }
    }
}

impl Tool for DemEval {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::dem_eval::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let truth = form
            .given(GROUND_TRUTH)
            .ok_or("Pick the ground-truth GeoTIFF.")?;
        let prediction = form
            .given(PREDICTION)
            .ok_or("Pick the predicted GeoTIFF.")?;
        let mut args = vec!["dem-eval".into(), truth.into(), prediction.into()];
        form.push_option(&mut args, RESULTS, "--results");
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::type_into;

    #[test]
    fn two_files_make_a_command() {
        let mut screen = DemEval::new();
        assert!(command(&screen).unwrap_err().contains("ground-truth"));
        type_into(&mut screen.form, GROUND_TRUTH, "truth.tif");
        type_into(&mut screen.form, PREDICTION, "guess.tif");
        assert_eq!(
            command(&screen).unwrap(),
            ["dem-eval", "truth.tif", "guess.tif"]
        );
        type_into(&mut screen.form, RESULTS, "Scores");
        assert_eq!(command(&screen).unwrap()[3..], ["--results", "Scores"]);
    }
}
