//! `mauro net image-to-map`'s screen: a training run, a picture, and where the map read off it goes.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const RUN: usize = 0;
const IMAGE: usize = 1;
const MAP_FILE: usize = 2;
const WEIGHTS: usize = 3;
const TOLERANCE: usize = 4;
const SAME_ANGLE: usize = 5;
const CROP: usize = 6;
const OVERLAP: usize = 7;
const RENDERED: usize = 8;
const RESOLUTION: usize = 9;
const SCALE: usize = 10;

pub struct NetImageToMap {
    form: Form,
}

impl NetImageToMap {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["net", "image-to-map"],
            "Read",
            vec![
                Field::path("Run folder", Pick::Folder).arg("run"),
                Field::path(
                    "Image",
                    Pick::Files(&["png", "jpg", "jpeg", "tif", "tiff", "bmp"]),
                )
                .arg("image"),
                Field::text("Map").arg("map_file"),
                Field::path("Weights", Pick::AnyFile).arg("weights"),
                Field::text("Tolerance").arg("tolerance").unit("px"),
                Field::text("Same angle").arg("same_angle").unit("turns"),
                Field::text("Crop").arg("crop").unit("px"),
                Field::text("Overlap").arg("overlap").unit("px"),
                Field::text("Rendered").arg("rendered"),
                Field::text("Resolution").arg("resolution").unit("px/m"),
                Field::text("Scale").arg("scale").unit("1:"),
            ],
        );
        form.open_picker(RUN);
        Self { form }
    }
}

impl Tool for NetImageToMap {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::net_image_to_map::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["net".to_string(), "image-to-map".to_string()];
        args.push(
            form.given(RUN)
                .ok_or("Pick the run folder (Enter on Run folder).")?
                .to_string(),
        );
        args.push(
            form.given(IMAGE)
                .ok_or("Pick the picture (Enter on Image).")?
                .to_string(),
        );
        if let Some(map_file) = form.given(MAP_FILE).filter(|_| !form.is_default(MAP_FILE)) {
            args.push(map_file.to_string());
        }
        form.push_option(&mut args, WEIGHTS, "--weights");
        form.push_option(&mut args, TOLERANCE, "--tolerance");
        form.push_option(&mut args, SAME_ANGLE, "--same-angle");
        form.push_option(&mut args, CROP, "--crop");
        form.push_option(&mut args, OVERLAP, "--overlap");
        form.push_option(&mut args, RENDERED, "--rendered");
        form.push_option(&mut args, RESOLUTION, "--resolution");
        form.push_option(&mut args, SCALE, "--scale");
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
        let mut screen = NetImageToMap::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, RUN, "trainings/run");
        type_into(&mut screen.form, IMAGE, "p.png");
        assert_eq!(
            command(&screen).unwrap(),
            ["net", "image-to-map", "trainings/run", "p.png"]
        );
        type_into(&mut screen.form, TOLERANCE, "2");
        assert_eq!(command(&screen).unwrap()[4..], ["--tolerance", "2"]);
    }
}
