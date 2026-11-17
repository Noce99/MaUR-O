//! `mauro tiles query`'s screen: an archive and the place to look at.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const ARCHIVE: usize = 0;
const AREA: usize = 1;
const POINT: usize = 2;
const SIZE: usize = 3;
const BOX: usize = 4;
const ZOOM: usize = 5;
const OBJECTS: usize = 6;

const AROUND: &str = "around a point";
const BBOX: &str = "a box";

pub struct TilesQuery {
    form: Form,
}

impl TilesQuery {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["tiles", "query"],
            "Query",
            vec![
                Field::path("Archive", Pick::Files(&["pmtiles"])).arg("archive"),
                Field::choice("Area", &[AROUND, BBOX]),
                Field::text("Point").arg("around").hint("lat,lon"),
                Field::text("Size").arg("size"),
                Field::text("Box")
                    .arg("bbox")
                    .hint("min_lon,min_lat,max_lon,max_lat"),
                Field::text("Zoom").arg("zoom").hint("the archive's finest"),
                Field::toggle("Objects").arg("objects"),
            ],
        );
        form.show(BOX, false);
        form.open_picker(ARCHIVE);
        Self { form }
    }
}

impl Tool for TilesQuery {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::tiles_query::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        if field == AREA {
            let around = self.form.choice(AREA) == AROUND;
            self.form.show(POINT, around);
            self.form.show(SIZE, around);
            self.form.show(BOX, !around);
        }
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let archive = form
            .given(ARCHIVE)
            .ok_or("Pick the archive (Enter on Archive).")?;
        let mut args = vec!["tiles".into(), "query".into(), archive.to_string()];
        if form.choice(AREA) == AROUND {
            let point = form
                .given(POINT)
                .ok_or("Give the point to look around, as lat,lon.")?;
            args.extend(["--around".to_string(), point.to_string()]);
            form.push_option(&mut args, SIZE, "--size");
        } else {
            let area = form
                .given(BOX)
                .ok_or("Give the box, as min_lon,min_lat,max_lon,max_lat.")?;
            args.extend(["--bbox".to_string(), area.to_string()]);
        }
        form.push_option(&mut args, ZOOM, "--zoom");
        form.push_flag(&mut args, OBJECTS, "--objects");
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
    fn a_point_or_a_box_makes_a_command() {
        let mut screen = TilesQuery::new();
        assert!(command(&screen).unwrap_err().contains("archive"));
        type_into(&mut screen.form, ARCHIVE, "w.pmtiles");
        assert!(command(&screen).unwrap_err().contains("point"));
        type_into(&mut screen.form, POINT, "44.5,11.3");
        assert_eq!(
            command(&screen).unwrap(),
            ["tiles", "query", "w.pmtiles", "--around", "44.5,11.3"]
        );
        // Switching to a box hides the point and asks for the box.
        press(&mut screen.form, AREA, KeyCode::Right);
        screen.changed(AREA);
        assert!(!screen.form.shown(POINT));
        assert!(command(&screen).unwrap_err().contains("box"));
        type_into(&mut screen.form, BOX, "11,44,12,45");
        assert_eq!(
            command(&screen).unwrap(),
            ["tiles", "query", "w.pmtiles", "--bbox", "11,44,12,45"]
        );
    }
}
