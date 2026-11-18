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
const CONTENT: usize = 8;
const RESOLUTION: usize = 9;
const MIN_RESOLUTION: usize = 10;
const MAX_RESOLUTION: usize = 11;
const FRAME: usize = 12;
const NO_IMAGES: usize = 13;
const NO_GT: usize = 14;
const IOF_RULES: usize = 15;
const SEED: usize = 16;

/// What the maps are drawn with, and so what their labels say. A step up is
/// the one before it and more: the labels of the areas are always there.
const AREAS: &str = "areas";
const AREAS_AND_LINES: &str = "areas and lines";

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
                Field::choice("Content", &[AREAS, AREAS_AND_LINES]).help(
                    "What the maps hold, and so what their labels say. areas: the ground cover \
                     alone, one label per pixel. areas and lines: the ground cover with a line \
                     along some of the cell sides, labelled as a second layer which leaves the \
                     ground under the line as it was.",
                ),
                Field::text("Resolution").arg("resolution").unit("px/m"),
                Field::text("Min resolution")
                    .arg("min_resolution")
                    .unit("px/m"),
                Field::text("Max resolution")
                    .arg("max_resolution")
                    .unit("px/m"),
                Field::text("Frame").arg("frame").unit("m"),
                Field::toggle("No images").arg("no_images"),
                Field::toggle("No labels").arg("no_gt"),
                Field::toggle("IOF rules").arg("iof_rules"),
                Field::text("Seed").arg("seed"),
            ],
        );
        form.open_picker(SYMBOL_SET);
        let mut screen = Self { form };
        screen.update();
        screen
    }

    fn lines(&self) -> bool {
        self.form.choice(CONTENT) == AREAS_AND_LINES
    }

    /// Shows what the content picked draws, and hides what it does not: the
    /// sides left empty are for lines, and the see-through areas and the point
    /// symbols are not drawn by either of the contents so far.
    fn update(&mut self) {
        let lines = self.lines();
        self.form.show(EMPTY_SIDES, lines);
        self.form.show(TRANSPARENT, false);
        self.form.show(POINTS, false);
    }
}

impl Tool for NetDataset {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn changed(&mut self, field: usize) {
        if field == CONTENT {
            self.update();
        }
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
        if self.lines() {
            // The see-through areas and the points would be drawn and left
            // unlabelled, which the generator refuses.
            args.extend(
                [
                    "--line-labels",
                    "--transparent-areas",
                    "0",
                    "--point-symbols",
                    "0",
                ]
                .map(str::to_string),
            );
        } else {
            args.push("--just-opaque-areas".to_string());
        }
        form.push_option(&mut args, RESOLUTION, "--resolution");
        form.push_option(&mut args, MIN_RESOLUTION, "--min-resolution");
        form.push_option(&mut args, MAX_RESOLUTION, "--max-resolution");
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
    use crate::tui::form::tests::{press, type_into};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn the_required_arguments_make_a_command() {
        let mut screen = NetDataset::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        assert_eq!(
            command(&screen).unwrap(),
            ["net", "dataset", "a.omap", "--just-opaque-areas"]
        );
        type_into(&mut screen.form, MAPS, "5");
        assert_eq!(command(&screen).unwrap()[3..5], ["--maps", "5"]);
    }

    /// Picks the next content on the screen, as a key on its field does.
    fn next_content(screen: &mut NetDataset) {
        press(&mut screen.form, CONTENT, KeyCode::Right);
        screen.changed(CONTENT);
    }

    #[test]
    fn areas_alone_is_the_default_and_draws_nothing_over_the_ground() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        let args = command(&screen).unwrap();
        assert!(
            args.contains(&"--just-opaque-areas".to_string()),
            "{args:?}"
        );
        assert!(!args.contains(&"--line-labels".to_string()), "{args:?}");
        assert!(!screen.form.shown(EMPTY_SIDES));
    }

    #[test]
    fn areas_and_lines_labels_the_lines_and_draws_nothing_else_over_them() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        next_content(&mut screen);
        assert_eq!(screen.form.choice(CONTENT), AREAS_AND_LINES);
        let args = command(&screen).unwrap();
        assert!(
            !args.contains(&"--just-opaque-areas".to_string()),
            "{args:?}"
        );
        assert_eq!(
            args[3..],
            [
                "--line-labels",
                "--transparent-areas",
                "0",
                "--point-symbols",
                "0"
            ]
        );
        assert!(screen.form.shown(EMPTY_SIDES));
    }

    #[test]
    fn what_is_after_the_content_is_still_what_it_says() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        next_content(&mut screen);
        type_into(&mut screen.form, RESOLUTION, "2");
        type_into(&mut screen.form, SEED, "7");
        let args = command(&screen).unwrap();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|at| &args[at + 1]);
        assert_eq!(after("--resolution").map(String::as_str), Some("2"));
        assert_eq!(after("--seed").map(String::as_str), Some("7"));
    }

    /// A range of densities is two options, and is left out until asked for.
    #[test]
    fn a_range_of_resolutions_is_passed_on_when_given() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        let args = command(&screen).unwrap();
        assert!(!args.iter().any(|a| a.contains("-resolution")), "{args:?}");

        type_into(&mut screen.form, MIN_RESOLUTION, "1");
        type_into(&mut screen.form, MAX_RESOLUTION, "6");
        let args = command(&screen).unwrap();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|at| &args[at + 1]);
        assert_eq!(after("--min-resolution").map(String::as_str), Some("1"));
        assert_eq!(after("--max-resolution").map(String::as_str), Some("6"));
    }
}
