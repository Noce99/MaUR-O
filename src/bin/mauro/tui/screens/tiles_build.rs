//! `mauro tiles build`'s screen: the maps, the archive they go into, and how
//! the tiles are cut.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

use super::MAP_EXTENSIONS;

const MAPS: usize = 0;
const OUTPUT: usize = 1;
const STANDARD: usize = 2;
const ZOOM: usize = 3;
const MIN_ZOOM: usize = 4;
const BUFFER: usize = 5;
const GRID_CM: usize = 6;

pub struct TilesBuild {
    form: Form,
}

impl TilesBuild {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["tiles", "build"],
            "Build",
            vec![
                Field::paths("Maps", Pick::Files(MAP_EXTENSIONS)).arg("maps"),
                Field::text("Archive").arg("output").hint("world.pmtiles"),
                Field::choice("Standard", &["auto", "isom", "issprom"]).arg("standard"),
                Field::text("Zoom").arg("zoom"),
                Field::text("Min zoom").arg("min_zoom"),
                Field::text("Buffer").arg("buffer"),
                Field::text("Grid").arg("grid_cm").unit("cm"),
            ],
        );
        form.open_picker(MAPS);
        Self { form }
    }
}

impl Tool for TilesBuild {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::tiles_build::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let maps: Vec<&str> = form
            .text(MAPS)
            .split(';')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .collect();
        if maps.is_empty() {
            return Err("Pick at least one map (Enter on Maps).".into());
        }
        let output = form
            .given(OUTPUT)
            .ok_or("Say which archive to write (Archive).")?;
        let mut args = vec!["tiles".into(), "build".into()];
        args.extend(["--output".to_string(), output.to_string()]);
        if form.choice(STANDARD) != "auto" {
            args.extend(["--standard".to_string(), form.choice(STANDARD).to_string()]);
        }
        form.push_option(&mut args, ZOOM, "--zoom");
        form.push_option(&mut args, MIN_ZOOM, "--min-zoom");
        form.push_option(&mut args, BUFFER, "--buffer");
        form.push_option(&mut args, GRID_CM, "--grid-cm");
        args.extend(maps.into_iter().map(String::from));
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::type_into;

    #[test]
    fn maps_and_an_archive_make_a_command() {
        let mut screen = TilesBuild::new();
        assert!(command(&screen).unwrap_err().contains("at least one map"));
        type_into(&mut screen.form, MAPS, "a.omap;b.omap");
        assert!(command(&screen).unwrap_err().contains("archive"));
        type_into(&mut screen.form, OUTPUT, "w.pmtiles");
        assert_eq!(
            command(&screen).unwrap(),
            [
                "tiles",
                "build",
                "--output",
                "w.pmtiles",
                "a.omap",
                "b.omap"
            ]
        );
        type_into(&mut screen.form, ZOOM, "12");
        assert_eq!(command(&screen).unwrap()[4..6], ["--zoom", "12"]);
    }
}
