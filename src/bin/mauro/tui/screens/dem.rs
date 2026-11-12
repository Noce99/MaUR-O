//! `mauro dem`'s screen: the map whose contours to read, and the run's
//! options, the map's summary beside them.

use std::path::Path;

use ratatui::text::Line;

use super::{number, Loading, MapSummary, MAP_EXTENSIONS};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const MAP: usize = 0;
const OUTPUT: usize = 1;
const RESULTS: usize = 2;
const CONFIG: usize = 3;
const EQUIDISTANCE: usize = 4;
const SVG: usize = 5;

pub struct Dem {
    form: Form,
    map: Loading<MapSummary>,
}

impl Dem {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["dem"],
            "Extract",
            vec![
                Field::path("Map", Pick::Files(MAP_EXTENSIONS)).arg("map_file"),
                Field::text("Output")
                    .arg("output_file")
                    .hint("the map's name, with .tif"),
                Field::path("Results", Pick::Folder).arg("results"),
                Field::path("Config", Pick::Files(&["conf"])).arg("config"),
                Field::text("Equidistance")
                    .arg("equidistance")
                    .unit("m between contours"),
                Field::toggle("Diagnostics").arg("create_svg"),
            ],
        );
        form.open_picker(MAP);
        Self {
            form,
            map: Loading::None,
        }
    }
}

impl Tool for Dem {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::dem::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        if field == MAP {
            let map = Path::new(self.form.text(MAP)).to_path_buf();
            self.map = if map.is_file() {
                Loading::start(move || MapSummary::read(&map))
            } else {
                Loading::None
            };
        }
    }

    fn tick(&mut self) {
        self.map.poll();
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let map = form.given(MAP).ok_or("Pick a map first (Enter on Map).")?;
        let mut args = vec!["dem".to_string(), map.to_string()];
        if let Some(output) = form.given(OUTPUT) {
            args.push(output.to_string());
        }
        form.push_option(&mut args, RESULTS, "--results");
        form.push_option(&mut args, CONFIG, "--config");
        if number(form.text(EQUIDISTANCE), "Equidistance")? <= 0.0 {
            return Err("The equidistance must be greater than zero.".to_string());
        }
        form.push_option(&mut args, EQUIDISTANCE, "--equidistance");
        form.push_flag(&mut args, SVG, "--create_svg");
        Ok(args)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        let lines = self
            .map
            .status("No map picked yet.")
            .unwrap_or_else(|| self.map.ready().map(MapSummary::lines).unwrap_or_default());
        Some(("The map", lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::{press, type_into};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn a_map_makes_a_command_and_options_add_to_it() {
        let mut screen = Dem::new();
        assert!(command(&screen).is_err());
        type_into(&mut screen.form, MAP, "tests/data/contours.xmap");
        screen.changed(MAP);
        assert_eq!(
            command(&screen).unwrap(),
            ["dem", "tests/data/contours.xmap"]
        );
        type_into(&mut screen.form, EQUIDISTANCE, "2.5");
        press(&mut screen.form, SVG, KeyCode::Enter);
        assert_eq!(
            command(&screen).unwrap()[2..],
            ["--equidistance", "2.5", "--create_svg"]
        );
        type_into(&mut screen.form, EQUIDISTANCE, "0");
        assert!(command(&screen).unwrap_err().contains("greater than zero"));

        screen.map.wait();
        assert!(screen.map.ready().is_some());
    }
}
