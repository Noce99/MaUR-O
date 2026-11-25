//! `mauro contours`' screen: the map, its terrain model, and how to draw
//! the contours.

use super::{number, MAP_EXTENSIONS};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const TIFF: &[&str] = &["tif", "tiff"];

const MAP: usize = 0;
const DTM: usize = 1;
const OUTPUT: usize = 2;
const EQUIDISTANCE: usize = 3;
const INDEX: usize = 4;
const FORM_LINES: usize = 5;
const SMOOTH: usize = 6;
const MIN_LENGTH: usize = 7;

pub struct Contours {
    form: Form,
}

impl Contours {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["contours"],
            "Draw",
            vec![
                Field::path("Map", Pick::Files(MAP_EXTENSIONS)).arg("map_file"),
                Field::path("Terrain model", Pick::Files(TIFF)).arg("dtm"),
                Field::text("Output")
                    .arg("output")
                    .hint("the map's name, with _contours.omap"),
                Field::text("Equidistance").arg("equidistance").unit("m"),
                Field::text("Index every").arg("index").unit("contours"),
                Field::toggle("Form lines").arg("form_lines"),
                Field::text("Smoothing")
                    .arg("smooth")
                    .unit("m on the ground"),
                Field::text("Shortest line")
                    .arg("min_length")
                    .unit("m on the ground"),
            ],
        );
        form.open_picker(MAP);
        Self { form }
    }
}

impl Tool for Contours {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::contours::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let map = form.given(MAP).ok_or("Pick a map first (Enter on Map).")?;
        let dtm = form
            .given(DTM)
            .ok_or("Pick the terrain model (an elevation GeoTIFF).")?;
        let mut args = vec!["contours".to_string(), map.to_string(), dtm.to_string()];
        form.push_option(&mut args, OUTPUT, "--output");
        if number(form.text(EQUIDISTANCE), "Equidistance")? <= 0.0 {
            return Err("The equidistance must be greater than zero.".to_string());
        }
        form.push_option(&mut args, EQUIDISTANCE, "--equidistance");
        let index = number(form.text(INDEX), "Index every")?;
        if index < 0.0 || index.fract() != 0.0 {
            return Err("Index every must be a whole number, 0 for none.".to_string());
        }
        form.push_option(&mut args, INDEX, "--index");
        form.push_flag(&mut args, FORM_LINES, "--form-lines");
        if number(form.text(SMOOTH), "Smoothing")? < 0.0 {
            return Err("The smoothing cannot be negative.".to_string());
        }
        form.push_option(&mut args, SMOOTH, "--smooth");
        if number(form.text(MIN_LENGTH), "Shortest line")? < 0.0 {
            return Err("The shortest line cannot be negative.".to_string());
        }
        form.push_option(&mut args, MIN_LENGTH, "--min-length");
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
    fn a_map_and_a_model_make_a_command() {
        let mut screen = Contours::new();
        assert!(command(&screen).unwrap_err().contains("map"));
        type_into(&mut screen.form, MAP, "forest.omap");
        assert!(command(&screen).unwrap_err().contains("terrain model"));
        type_into(&mut screen.form, DTM, "forest.tif");
        assert_eq!(
            command(&screen).unwrap(),
            ["contours", "forest.omap", "forest.tif"]
        );
        type_into(&mut screen.form, EQUIDISTANCE, "2.5");
        type_into(&mut screen.form, INDEX, "4");
        press(&mut screen.form, FORM_LINES, KeyCode::Enter);
        assert_eq!(
            command(&screen).unwrap()[3..],
            ["--equidistance", "2.5", "--index", "4", "--form-lines"]
        );
        type_into(&mut screen.form, EQUIDISTANCE, "0");
        assert!(command(&screen).is_err());
        type_into(&mut screen.form, EQUIDISTANCE, "5");
        type_into(&mut screen.form, INDEX, "1.5");
        assert!(command(&screen).is_err());
    }
}
