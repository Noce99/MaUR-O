//! `mauro net grid-to-map`'s screen: a symbol set, the labels written beside a dataset's image, and where the map goes.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

use super::MAP_EXTENSIONS;

const SYMBOL_SET: usize = 0;
const LABELS: usize = 1;
const MAP_FILE: usize = 2;
const IMAGE: usize = 3;
const TOLERANCE: usize = 4;
const SAME_ANGLE: usize = 5;
const LAYOUT_SIZE: usize = 6;
const CELL_SIZE: usize = 7;
const FRAME: usize = 8;
const RESOLUTION: usize = 9;
const SCALE: usize = 10;

pub struct NetGridToMap {
    form: Form,
}

impl NetGridToMap {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["net", "grid-to-map"],
            "Convert",
            vec![
                Field::path("Symbol set", Pick::Files(MAP_EXTENSIONS)).arg("symbol_set"),
                Field::path("Labels", Pick::Files(&["bin"])).arg("labels"),
                Field::text("Map").arg("map_file"),
                Field::text("Image").arg("image"),
                Field::text("Tolerance").arg("tolerance").unit("cells"),
                Field::text("Same angle").arg("same_angle").unit("turns"),
                Field::text("Layout size").arg("layout_size").unit("cells"),
                Field::text("Cell size")
                    .arg("background_cell_size")
                    .unit("m"),
                Field::text("Frame").arg("frame").unit("m"),
                Field::text("Resolution").arg("resolution").unit("px/m"),
                Field::text("Scale").arg("scale").unit("1:"),
            ],
        );
        form.open_picker(SYMBOL_SET);
        Self { form }
    }
}

impl Tool for NetGridToMap {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::net_grid_to_map::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["net".to_string(), "grid-to-map".to_string()];
        args.push(
            form.given(SYMBOL_SET)
                .ok_or("Pick the symbol set (Enter on Symbol set).")?
                .to_string(),
        );
        args.push(
            form.given(LABELS)
                .ok_or("Pick the labels file (Enter on Labels).")?
                .to_string(),
        );
        if let Some(map_file) = form.given(MAP_FILE).filter(|_| !form.is_default(MAP_FILE)) {
            args.push(map_file.to_string());
        }
        form.push_option(&mut args, IMAGE, "--image");
        form.push_option(&mut args, TOLERANCE, "--tolerance");
        form.push_option(&mut args, SAME_ANGLE, "--same-angle");
        form.push_option(&mut args, LAYOUT_SIZE, "--layout-size");
        form.push_option(&mut args, CELL_SIZE, "--background-cell-size");
        form.push_option(&mut args, FRAME, "--frame");
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
        let mut screen = NetGridToMap::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        type_into(&mut screen.form, LABELS, "g.bin");
        assert_eq!(
            command(&screen).unwrap(),
            ["net", "grid-to-map", "a.omap", "g.bin"]
        );
        type_into(&mut screen.form, TOLERANCE, "3");
        assert_eq!(command(&screen).unwrap()[4..], ["--tolerance", "3"]);
    }
}
