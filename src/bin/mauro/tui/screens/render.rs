//! `mauro render`'s screen: the map picked from the disk, the image's
//! options, and beside them what the map is and how large the image will
//! be, before anything is drawn.

use std::path::Path;

use ratatui::style::Stylize;
use ratatui::text::Line;

use super::{number, Loading, MapSummary, MAP_EXTENSIONS};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

/// The output formats, by suffix, in the order ← → goes through them.
const FORMATS: [&str; 5] = ["png", "jpg", "tif", "bmp", "svg"];

/// An image this large (in megapixels) is pointed out before it is drawn.
const LARGE_MEGAPIXELS: f64 = 200.0;

const MAP: usize = 0;
const FORMAT: usize = 1;
const OUTPUT: usize = 2;
const AREA: usize = 3;
const RESOLUTION: usize = 4;
const FRAME: usize = 5;
const WINDOW: usize = 6;

pub struct Render {
    form: Form,
    map: Loading<MapSummary>,
}

impl Render {
    pub fn new() -> Self {
        let mut screen = Self {
            form: Form::new(
                &["render"],
                "Render",
                vec![
                    Field::path("Map", Pick::Files(MAP_EXTENSIONS)).arg("map_file"),
                    Field::choice("Format", &FORMATS),
                    Field::text("Output")
                        .arg("image_file")
                        .hint("the map's name, with the format's suffix"),
                    Field::choice("Area", &["whole map", "window"]),
                    Field::text("Resolution")
                        .arg("resolution")
                        .unit("px per m on the ground"),
                    Field::text("Frame")
                        .arg("frame")
                        .unit("m on the ground, each side"),
                    Field::text("Window")
                        .arg("window")
                        .hint("left,top,right,bottom in mm on the paper"),
                ],
            ),
            map: Loading::None,
        };
        screen.update();
        screen.form.open_picker(MAP);
        screen
    }

    fn svg(&self) -> bool {
        self.form.choice(FORMAT) == "svg"
    }

    fn whole_map(&self) -> bool {
        self.form.choice(AREA) == "whole map"
    }

    /// Shows the fields the format and the area call for: an SVG has no
    /// pixels and no window.
    fn update(&mut self) {
        let (svg, whole) = (self.svg(), self.whole_map());
        self.form.show(AREA, !svg);
        self.form.show(RESOLUTION, !svg);
        self.form.show(FRAME, svg || whole);
        self.form.show(WINDOW, !svg && !whole);
    }

    /// How large the image will be, in pixels and on the ground, once the
    /// map and the options say.
    fn size(&self, map: &MapSummary) -> Option<String> {
        let mm_per_meter = 1000.0 / map.scale as f64;
        let extent = if self.svg() || self.whole_map() {
            let frame = number(self.form.text(FRAME), "Frame").ok()? * mm_per_meter;
            map.extent.adjusted(-frame, -frame, frame, frame)
        } else {
            crate::render::parse_window(self.form.text(WINDOW)).ok()?
        };
        let ground = format!(
            "{:.0} × {:.0} m",
            extent.width() / mm_per_meter,
            extent.height() / mm_per_meter
        );
        if self.svg() {
            return Some(format!("vector, {ground}"));
        }
        let resolution = number(self.form.text(RESOLUTION), "Resolution").ok()?;
        let pixel_per_mm = resolution * map.scale as f64 / 1000.0;
        let (width, height) = (
            (extent.width() * pixel_per_mm).round(),
            (extent.height() * pixel_per_mm).round(),
        );
        let megapixels = width * height / 1e6;
        let mut text = format!("{width:.0} × {height:.0} px ({megapixels:.1} MP), {ground}");
        if megapixels > LARGE_MEGAPIXELS {
            text.push_str(" — large!");
        }
        Some(text)
    }
}

impl Tool for Render {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::render::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        match field {
            MAP => {
                let map = Path::new(self.form.text(MAP)).to_path_buf();
                let output = map.with_extension(self.form.choice(FORMAT));
                self.form.set_text(OUTPUT, output.display().to_string());
                self.map = if map.is_file() {
                    Loading::start(move || MapSummary::read(&map))
                } else {
                    Loading::None
                };
            }
            FORMAT => {
                // The output keeps its name and takes the new suffix.
                if !self.form.text(OUTPUT).is_empty() {
                    let renamed =
                        Path::new(self.form.text(OUTPUT)).with_extension(self.form.choice(FORMAT));
                    self.form.set_text(OUTPUT, renamed.display().to_string());
                }
                self.update();
            }
            AREA => self.update(),
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.map.poll();
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let map = form.given(MAP).ok_or("Pick a map first (Enter on Map).")?;
        let mut args = vec!["render".to_string(), map.to_string()];
        if let Some(output) = form.given(OUTPUT) {
            args.push(output.to_string());
        }
        if !self.svg() && number(form.text(RESOLUTION), "Resolution")? <= 0.0 {
            return Err("The resolution must be greater than zero.".to_string());
        }
        form.push_option(&mut args, RESOLUTION, "-r");
        if form.shown(FRAME) && number(form.text(FRAME), "Frame")? < 0.0 {
            return Err("The frame cannot be negative.".to_string());
        }
        form.push_option(&mut args, FRAME, "-f");
        if form.shown(WINDOW) {
            crate::render::parse_window(form.text(WINDOW))?;
            form.push_option(&mut args, WINDOW, "-w");
        }
        Ok(args)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        let lines = self.map.status("No map picked yet.").unwrap_or_else(|| {
            let map = self.map.ready().expect("ready when there is no status");
            let mut lines = map.lines();
            lines.push(Line::default());
            lines.push(Line::from("The image").bold());
            lines.push(match self.size(map) {
                Some(size) => Line::from(size),
                None => Line::from("(the options are not numbers yet)").dim(),
            });
            lines
        });
        Some(("The map", lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::tests::{press, type_into};
    use crate::tui::form::{command, handle};
    use crate::tui::Action;
    use ratatui::crossterm::event::KeyCode;

    /// The screen with `tests/data/shapes.xmap` picked, and read.
    fn with_map() -> Render {
        let mut screen = Render::new();
        screen
            .form
            .handle(crate::tui::form::tests::key(KeyCode::Esc)); // the picker
        type_into(&mut screen.form, MAP, "tests/data/shapes.xmap");
        screen.changed(MAP);
        screen.map.wait();
        screen
    }

    #[test]
    fn no_map_is_no_command() {
        let mut screen = Render::new();
        screen
            .form
            .handle(crate::tui::form::tests::key(KeyCode::Esc));
        assert!(command(&screen).unwrap_err().contains("Pick a map"));
        assert!(matches!(
            handle(&mut screen, crate::tui::form::tests::ctrl_r()),
            Action::None
        ));
    }

    #[test]
    fn the_defaults_make_a_short_command_mauro_accepts() {
        let screen = with_map();
        assert_eq!(
            command(&screen).unwrap(),
            ["render", "tests/data/shapes.xmap", "tests/data/shapes.png"]
        );
        let map = screen.map.ready().unwrap();
        assert!(screen.size(map).unwrap().contains(" px "));
    }

    #[test]
    fn the_format_renames_the_output_and_svg_drops_the_pixels() {
        let mut screen = with_map();
        press(&mut screen.form, FORMAT, KeyCode::Left);
        screen.changed(FORMAT);
        assert_eq!(screen.form.choice(FORMAT), "svg");
        type_into(&mut screen.form, FRAME, "10");
        assert_eq!(
            command(&screen).unwrap(),
            [
                "render",
                "tests/data/shapes.xmap",
                "tests/data/shapes.svg",
                "-f",
                "10"
            ]
        );
        assert!(!screen.form.shown(RESOLUTION));
        let map = screen.map.ready().unwrap();
        assert!(screen.size(map).unwrap().starts_with("vector"));
    }

    #[test]
    fn a_window_replaces_the_frame() {
        let mut screen = with_map();
        press(&mut screen.form, AREA, KeyCode::Right);
        screen.changed(AREA);
        assert!(screen.form.shown(WINDOW));
        assert!(!screen.form.shown(FRAME));
        assert!(command(&screen).unwrap_err().contains("--window"));
        type_into(&mut screen.form, WINDOW, "0,0,40,20");
        type_into(&mut screen.form, RESOLUTION, "6");
        let args = command(&screen).unwrap();
        assert_eq!(args[3..], ["-r", "6", "-w", "0,0,40,20"]);
        // 40 mm on the paper, at 1:scale and 6 px per m.
        let map = screen.map.ready().unwrap();
        let width = (40.0 * 6.0 * map.scale as f64 / 1000.0).round();
        assert!(screen
            .size(map)
            .unwrap()
            .starts_with(&format!("{width:.0} × ")));
    }

    #[test]
    fn bad_numbers_are_caught_before_running() {
        let mut screen = with_map();
        type_into(&mut screen.form, RESOLUTION, "0");
        assert!(command(&screen).unwrap_err().contains("greater than zero"));
        type_into(&mut screen.form, RESOLUTION, "abc");
        assert!(command(&screen).unwrap_err().contains("not a number"));
        type_into(&mut screen.form, RESOLUTION, "2.5");
        type_into(&mut screen.form, FRAME, "-1");
        assert!(command(&screen).unwrap_err().contains("negative"));
    }

    #[test]
    fn running_hands_over_the_command_and_its_exit_codes() {
        let mut screen = with_map();
        let ctrl_r = crate::tui::form::tests::ctrl_r();
        let Action::Run(spec) = handle(&mut screen, ctrl_r) else {
            panic!("no run")
        };
        assert_eq!(spec.args[0], "render");
        assert_eq!((spec.explain)(2), Some("the map could not be read"));
    }

    #[test]
    fn an_unreadable_map_says_so() {
        let mut screen = Render::new();
        screen.map = Loading::start(|| MapSummary::read(Path::new("tests/data/no_such.omap")));
        screen.map.wait();
        assert!(matches!(screen.map, Loading::Failed(_)));
    }
}
