//! `mauro net dataset`'s screen: the symbol set a dataset of random maps is drawn with, and how the maps are made.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

use super::MAP_EXTENSIONS;

const SYMBOL_SET: usize = 0;
const FOLDER: usize = 1;
const MAPS: usize = 2;
const LAYOUT_SIZE: usize = 3;
const CELL_SIZE: usize = 4;
const EMPTY_SIDES: usize = 5;
const TRANSPARENT: usize = 6;
const POINTS: usize = 7;
const JUST_OPAQUE: usize = 8;
const RESOLUTION: usize = 9;
const FRAME: usize = 10;
const NO_IMAGES: usize = 11;
const NO_GT: usize = 12;
const IOF_RULES: usize = 13;
const SEED: usize = 14;

pub struct NetDataset {
    form: Form,
}

impl NetDataset {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["net", "dataset"],
            "Generate",
            vec![
                Field::path("Symbol set", Pick::Files(MAP_EXTENSIONS)).arg("symbol_set"),
                Field::path("Folder", Pick::Folder).arg("folder"),
                Field::text("Maps").arg("maps"),
                Field::text("Layout size").arg("layout_size").unit("cells"),
                Field::text("Cell size")
                    .arg("background_cell_size")
                    .unit("m"),
                Field::text("Empty sides").arg("empty_sides"),
                Field::text("Transparent").arg("transparent_areas"),
                Field::text("Point symbols").arg("point_symbols"),
                Field::toggle("Just opaque areas").arg("just_opaque_areas"),
                Field::text("Resolution").arg("resolution").unit("px/m"),
                Field::text("Frame").arg("frame").unit("m"),
                Field::toggle("No images").arg("no_images"),
                Field::toggle("No labels").arg("no_gt"),
                Field::toggle("IOF rules").arg("iof_rules"),
                Field::text("Seed").arg("seed"),
            ],
        );
        form.open_picker(SYMBOL_SET);
        Self { form }
    }
}

impl Tool for NetDataset {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::net_dataset::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["net".to_string(), "dataset".to_string()];
        args.push(
            form.given(SYMBOL_SET)
                .ok_or("Pick the symbol set (Enter on Symbol set).")?
                .to_string(),
        );
        if let Some(folder) = form.given(FOLDER).filter(|_| !form.is_default(FOLDER)) {
            args.push(folder.to_string());
        }
        form.push_option(&mut args, MAPS, "--maps");
        form.push_option(&mut args, LAYOUT_SIZE, "--layout-size");
        form.push_option(&mut args, CELL_SIZE, "--background-cell-size");
        form.push_option(&mut args, EMPTY_SIDES, "--empty-sides");
        form.push_option(&mut args, TRANSPARENT, "--transparent-areas");
        form.push_option(&mut args, POINTS, "--point-symbols");
        form.push_flag(&mut args, JUST_OPAQUE, "--just-opaque-areas");
        form.push_option(&mut args, RESOLUTION, "--resolution");
        form.push_option(&mut args, FRAME, "--frame");
        form.push_flag(&mut args, NO_IMAGES, "--no-images");
        form.push_flag(&mut args, NO_GT, "--no-gt");
        form.push_flag(&mut args, IOF_RULES, "--iof-rules");
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
        let mut screen = NetDataset::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        assert_eq!(command(&screen).unwrap(), ["net", "dataset", "a.omap"]);
        type_into(&mut screen.form, MAPS, "5");
        assert_eq!(command(&screen).unwrap()[3..], ["--maps", "5"]);
    }
}
