//! `mauro net dataset`'s screen: the symbol set a dataset of random maps is drawn with, and how the maps are made.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

use super::{number, MAP_EXTENSIONS};

const SYMBOL_SET: usize = 0;
const FOLDER: usize = 1;
const MAPS: usize = 2;
const LAYOUT_SIZE: usize = 3;
const CELL_SIZE: usize = 4;
const CONTENT: usize = 5;
const EMPTY_SIDES: usize = 6;
const INTERIOR_LINES: usize = 7;
const TRANSPARENT: usize = 8;
const POINTS: usize = 9;
const MIN_RESOLUTION: usize = 10;
const MAX_RESOLUTION: usize = 11;
const FRAME: usize = 12;
const NO_IMAGES: usize = 13;
const NO_GT: usize = 14;
const IOF_RULES: usize = 15;
const SEED: usize = 16;
/// Never shown: the density the images are sized at, which the range the
/// two fields above give decides (see [`NetDataset::resolution`]).
const RESOLUTION: usize = 17;

/// What the maps are drawn with, and so what their labels say: `--content`'s
/// values. A step up is the one before it and more.
const ALL: &str = "all";
const AREAS: &str = "areas";
const AREAS_AND_LINES: &str = "areas-and-lines";
const AREAS_LINES_POINTS: &str = "areas-lines-points";

pub struct NetDataset {
    form: Form,
    /// Whether the defaults give a range of their own, rather than leaving
    /// it to `--resolution`.
    range_defaults: bool,
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
                Field::choice(
                    "Content",
                    &[ALL, AREAS, AREAS_AND_LINES, AREAS_LINES_POINTS],
                )
                .arg("content"),
                Field::text("Empty sides").arg("empty_sides"),
                Field::text("Interior lines").arg("interior_lines"),
                Field::text("Transparent").arg("transparent_areas"),
                Field::text("Point symbols").arg("point_symbols"),
                Field::text("Min resolution")
                    .arg("min_resolution")
                    .unit("px/m")
                    .help(RANGE_HELP),
                Field::text("Max resolution")
                    .arg("max_resolution")
                    .unit("px/m")
                    .help(RANGE_HELP),
                Field::text("Frame").arg("frame").unit("m"),
                Field::toggle("No images").arg("no_images"),
                Field::toggle("No labels").arg("no_gt"),
                Field::toggle("IOF rules").arg("iof_rules"),
                Field::text("Seed").arg("seed"),
                Field::text("Resolution").arg("resolution"),
            ],
        );
        // Without a range of their own, both ends are the one density.
        let range_defaults =
            form.default(MIN_RESOLUTION).is_some() || form.default(MAX_RESOLUTION).is_some();
        let resolution = form.default(RESOLUTION).unwrap_or("3").to_string();
        for end in [MIN_RESOLUTION, MAX_RESOLUTION] {
            if form.default(end).is_none() {
                form.set_default(end, &resolution);
            }
        }
        form.show(RESOLUTION, false);
        form.open_picker(SYMBOL_SET);
        let mut screen = Self {
            form,
            range_defaults,
        };
        screen.update();
        screen
    }

    fn content(&self) -> &str {
        self.form.choice_value(CONTENT)
    }

    /// The range of densities, lowest first, or what is wrong with it.
    fn range(&self) -> Result<(f64, f64), String> {
        let low = number(self.form.text(MIN_RESOLUTION), "Min resolution")?;
        let high = number(self.form.text(MAX_RESOLUTION), "Max resolution")?;
        if low <= 0.0 || high <= 0.0 {
            return Err("A resolution must be greater than zero.".to_string());
        }
        if low > high {
            return Err(format!(
                "Min resolution ({low}) is above Max resolution ({high})."
            ));
        }
        Ok((low, high))
    }

    /// The density the images are sized at, for a range: the default one
    /// where it lies in it, else the middle of it — the geometric mean, since
    /// the densities are drawn log-uniform — rounded to a thousandth.
    fn resolution(&self, low: f64, high: f64) -> String {
        let default = self.form.default(RESOLUTION).unwrap_or("3");
        match default.parse::<f64>() {
            Ok(r) if low <= r && r <= high => default.to_string(),
            _ => (((low * high).sqrt() * 1000.0).round() / 1000.0).to_string(),
        }
    }

    /// Shows what the content picked draws, and hides what it does not: the
    /// sides left empty and the lines through the cells are for lines, which
    /// the ground alone has none of,
    /// the see-through areas are drawn only by `all`, and the point symbols
    /// by `all` and `areas-lines-points`. A range upside down is drawn red.
    fn update(&mut self) {
        let content = self.content().to_string();
        self.form.show(EMPTY_SIDES, content != AREAS);
        self.form.show(INTERIOR_LINES, content != AREAS);
        self.form.show(TRANSPARENT, content == ALL);
        self.form
            .show(POINTS, content == ALL || content == AREAS_LINES_POINTS);
        let problem = self.range().err();
        self.form.set_problem(MIN_RESOLUTION, problem.clone());
        self.form.set_problem(MAX_RESOLUTION, problem);
    }
}

/// What the two ends of the density range are, said on both.
const RANGE_HELP: &str = "The densities the maps are drawn at, in pixels per meter of ground: \
    each map at one of its own, log-uniform between the two, over as much ground as the \
    image's pixels cover at it, so only the symbols show how dense a picture is (and the \
    U-Net trained on it learns to say). The same figure in both draws every map at that \
    one density. The images are sized at the default resolution where it lies in the \
    range, else at the range's middle.";

impl Tool for NetDataset {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn changed(&mut self, _field: usize) {
        self.update();
    }

    /// The two ends of the range are made the defaults together, with the
    /// density the images are sized at, which has to lie between them: one
    /// end alone could leave the other on the wrong side of it.
    fn save_default(&mut self, field: usize) -> Result<String, String> {
        if field != MIN_RESOLUTION && field != MAX_RESOLUTION {
            return self.form.save_default(field);
        }
        let (low, high) = self.range()?;
        let resolution = self.resolution(low, high);
        self.form.set_text(RESOLUTION, resolution.clone());
        self.form.save_default(RESOLUTION)?;
        self.form.save_default(MIN_RESOLUTION)?;
        self.form.save_default(MAX_RESOLUTION)?;
        self.range_defaults = true;
        Ok(format!(
            "The resolutions default to {low}–{high} px/m now, the images sized at {resolution}."
        ))
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
        if Some(self.content()) != form.default(CONTENT) {
            args.extend(["--content".to_string(), self.content().to_string()]);
        }
        form.push_option(&mut args, EMPTY_SIDES, "--empty-sides");
        form.push_option(&mut args, INTERIOR_LINES, "--interior-lines");
        form.push_option(&mut args, TRANSPARENT, "--transparent-areas");
        form.push_option(&mut args, POINTS, "--point-symbols");
        let (low, high) = self.range()?;
        let resolution = self.resolution(low, high);
        if Some(resolution.as_str()) != form.default(RESOLUTION) {
            args.extend(["--resolution".to_string(), resolution]);
        }
        // One density is --resolution alone, unless the defaults would make
        // a range of it.
        if low < high || self.range_defaults {
            form.push_option(&mut args, MIN_RESOLUTION, "--min-resolution");
            form.push_option(&mut args, MAX_RESOLUTION, "--max-resolution");
        }
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
    use crate::tui::form::tests::{press, type_into};
    use crate::tui::form::{command, State};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn the_required_arguments_make_a_command() {
        let mut screen = NetDataset::new();
        assert!(command(&screen).unwrap_err().contains("Pick"));
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        assert_eq!(command(&screen).unwrap(), ["net", "dataset", "a.omap"]);
        type_into(&mut screen.form, MAPS, "5");
        assert_eq!(command(&screen).unwrap()[3..5], ["--maps", "5"]);
    }

    /// Picks the next content on the screen, as a key on its field does.
    fn next_content(screen: &mut NetDataset) {
        press(&mut screen.form, CONTENT, KeyCode::Right);
        screen.changed(CONTENT);
    }

    #[test]
    fn the_content_is_mauros_own_option_and_its_default() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        assert_eq!(screen.form.choice_value(CONTENT), ALL);
        assert_eq!(screen.form.state(CONTENT), State::Default);
        // Everything is drawn: every overlay's option is there to set.
        for field in [EMPTY_SIDES, INTERIOR_LINES, TRANSPARENT, POINTS] {
            assert!(screen.form.shown(field));
        }
        type_into(&mut screen.form, TRANSPARENT, "0.3");
        let args = command(&screen).unwrap();
        assert_eq!(args[3..], ["--transparent-areas", "0.3"]);
    }

    #[test]
    fn areas_alone_draws_nothing_over_the_ground() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        next_content(&mut screen);
        assert_eq!(screen.form.choice_value(CONTENT), AREAS);
        assert_eq!(screen.form.state(CONTENT), State::Changed);
        assert_eq!(command(&screen).unwrap()[3..], ["--content", "areas"]);
        for field in [EMPTY_SIDES, INTERIOR_LINES, TRANSPARENT, POINTS] {
            assert!(!screen.form.shown(field));
        }
    }

    #[test]
    fn areas_and_lines_has_the_sides_to_leave_empty_and_nothing_else() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        next_content(&mut screen);
        next_content(&mut screen);
        assert_eq!(screen.form.choice_value(CONTENT), AREAS_AND_LINES);
        type_into(&mut screen.form, EMPTY_SIDES, "0.7");
        type_into(&mut screen.form, INTERIOR_LINES, "0.4");
        assert_eq!(
            command(&screen).unwrap()[3..],
            [
                "--content",
                "areas-and-lines",
                "--empty-sides",
                "0.7",
                "--interior-lines",
                "0.4"
            ]
        );
        assert!(!screen.form.shown(TRANSPARENT) && !screen.form.shown(POINTS));
    }

    #[test]
    fn areas_lines_and_points_has_the_sides_and_the_points_and_no_transparent_areas() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        for _ in 0..3 {
            next_content(&mut screen);
        }
        assert_eq!(screen.form.choice_value(CONTENT), AREAS_LINES_POINTS);
        type_into(&mut screen.form, POINTS, "0.8");
        assert_eq!(
            command(&screen).unwrap()[3..],
            ["--content", "areas-lines-points", "--point-symbols", "0.8"]
        );
        assert!(screen.form.shown(EMPTY_SIDES) && screen.form.shown(POINTS));
        assert!(!screen.form.shown(TRANSPARENT));
    }

    #[test]
    fn what_is_after_the_content_is_still_what_it_says() {
        let mut screen = NetDataset::new();
        type_into(&mut screen.form, SYMBOL_SET, "a.omap");
        next_content(&mut screen);
        type_into(&mut screen.form, MIN_RESOLUTION, "2");
        type_into(&mut screen.form, MAX_RESOLUTION, "2");
        type_into(&mut screen.form, SEED, "7");
        let args = command(&screen).unwrap();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|at| &args[at + 1]);
        assert_eq!(after("--resolution").map(String::as_str), Some("2"));
        assert!(!args.iter().any(|a| a.contains("-min-")), "{args:?}");
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
        assert_eq!(after("--resolution"), None, "the default lies in it");

        // A range the default is outside of sizes the images in its middle.
        type_into(&mut screen.form, MIN_RESOLUTION, "4");
        type_into(&mut screen.form, MAX_RESOLUTION, "9");
        let args = command(&screen).unwrap();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|at| &args[at + 1]);
        assert_eq!(after("--resolution").map(String::as_str), Some("6"));

        // Upside down, it is red and no command.
        type_into(&mut screen.form, MIN_RESOLUTION, "10");
        screen.changed(MIN_RESOLUTION);
        assert!(command(&screen).unwrap_err().contains("above"));
        assert!(matches!(screen.form.state(MAX_RESOLUTION), State::Error(_)));
    }

    #[test]
    fn empty_sides_come_after_the_content_they_belong_to() {
        const { assert!(CONTENT < EMPTY_SIDES) };
        let screen = NetDataset::new();
        assert!(!screen.form.shown(RESOLUTION), "the range says it");
    }
}
