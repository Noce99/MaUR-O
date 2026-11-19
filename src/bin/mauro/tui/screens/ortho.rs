//! `mauro ortho`'s screen: the map to add a photo to, and, once it is
//! picked, where it is on the earth and which services cover it — worked
//! out offline, so that the service is picked from those alone.

use std::path::Path;

use ratatui::style::Stylize;
use ratatui::text::Line;

use maur_o::orthophoto::{geographic_anchor, Service};
use maur_o::orthophoto_services::SERVICES;

use super::{number, pair, Loading};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

/// The service choice that leaves it to `mauro ortho`.
const BEST: &str = "the best for the map";

const MAP: usize = 0;
const OUTPUT: usize = 1;
const SERVICE: usize = 2;
const PIXEL: usize = 3;
const MARGIN: usize = 4;
const IMAGE: usize = 5;
const LIST: usize = 6;

/// Where the picked map is, and what covers it.
struct Place {
    lat: f64,
    lon: f64,
    epsg: Option<i32>,
    /// The services whose box holds the map, best first.
    services: Vec<&'static Service>,
}

impl Place {
    fn read(path: &Path) -> Result<Self, String> {
        let xml = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let (map, _) = maur_o::xml_reader::read_xml_map_str(&xml)?;
        let anchor = geographic_anchor(&xml)
            .filter(|_| map.georeferencing.is_some())
            .ok_or("The map is not georeferenced: it has no place on the earth.")?;
        Ok(Self {
            lat: anchor.lat,
            lon: anchor.lon,
            epsg: map
                .georeferencing
                .as_ref()
                .map(|g| g.epsg)
                .filter(|e| *e != 0),
            services: SERVICES
                .iter()
                .filter(|s| s.covers(anchor.lat, anchor.lon))
                .collect(),
        })
    }
}

pub struct Ortho {
    form: Form,
    place: Loading<Place>,
}

impl Ortho {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["ortho"],
            "Add the photo",
            vec![
                Field::path("Map", Pick::Files(&["omap", "xmap"]))
                    .arg("map_file")
                    .required(),
                Field::text("Output map")
                    .arg("output_map_file")
                    .hint("the map itself, changed in place"),
                Field::choice("Service", &[BEST]).arg("service").help(
                    "The service to download from: the best one for where the map is (the \
                     first, best first, with photos of all of it), or one of those whose area \
                     holds the map, listed beside once the map is picked.",
                ),
                Field::text("Pixel size")
                    .arg("pixel_size")
                    .unit("m per pixel")
                    .hint("the service's own"),
                Field::text("Margin").arg("margin").unit("m on the ground"),
                Field::text("Image")
                    .arg("image")
                    .hint("the map's name, with the service's id and .jpg"),
                Field::toggle("List services").arg("list_services"),
            ],
        );
        form.open_picker(MAP);
        Self {
            form,
            place: Loading::None,
        }
    }

    fn update(&mut self) {
        let listing = self.form.on(LIST);
        for field in [MAP, OUTPUT, SERVICE, PIXEL, MARGIN, IMAGE] {
            self.form.show(field, !listing);
        }
    }
}

impl Tool for Ortho {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::ortho::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        match field {
            MAP => {
                let map = Path::new(self.form.text(MAP)).to_path_buf();
                self.place = if map.is_file() {
                    Loading::start(move || Place::read(&map))
                } else {
                    Loading::None
                };
                self.form.set_choices(SERVICE, vec![BEST.to_string()]);
            }
            LIST => self.update(),
            _ => {}
        }
    }

    fn tick(&mut self) {
        let was_busy = matches!(self.place, Loading::Busy(_));
        self.place.poll();
        if was_busy {
            if let Some(place) = self.place.ready() {
                let choices = std::iter::once(BEST.to_string())
                    .chain(place.services.iter().map(|s| s.id.to_string()))
                    .collect();
                self.form.set_choices(SERVICE, choices);
            }
        }
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        if form.on(LIST) {
            return Ok(vec!["ortho".into(), "--list-services".into()]);
        }
        let map = form.given(MAP).ok_or("Pick a map first (Enter on Map).")?;
        let mut args = vec!["ortho".to_string(), map.to_string()];
        if let Some(output) = form.given(OUTPUT) {
            args.push(output.to_string());
        }
        if form.choice(SERVICE) != BEST {
            args.extend(["--service".to_string(), form.choice(SERVICE).to_string()]);
        }
        if let Some(pixel) = form.given(PIXEL) {
            if number(pixel, "Pixel size")? <= 0.0 {
                return Err("The pixel size must be greater than zero.".to_string());
            }
        }
        form.push_option(&mut args, PIXEL, "--pixel-size");
        if number(form.text(MARGIN), "Margin")? < 0.0 {
            return Err("The margin cannot be negative.".to_string());
        }
        form.push_option(&mut args, MARGIN, "--margin");
        form.push_option(&mut args, IMAGE, "--image");
        Ok(args)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        if self.form.on(LIST) {
            return None;
        }
        let lines = self.place.status("No map picked yet.").unwrap_or_else(|| {
            let place = self.place.ready().expect("ready when there is no status");
            let mut lines = vec![
                pair("Where", format!("{:.5}, {:.5}", place.lat, place.lon)),
                pair(
                    "CRS",
                    place
                        .epsg
                        .map_or("none named".to_string(), |e| format!("EPSG:{e}")),
                ),
                Line::default(),
            ];
            if place.services.is_empty() {
                lines.push(Line::from("No service covers it.").red());
            } else {
                lines.push(Line::from("Covered by, best first").bold());
                for s in &place.services {
                    lines.push(Line::from(format!(
                        "{}  {}m {}",
                        s.id, s.ground_resolution, s.years
                    )));
                }
            }
            lines
        });
        Some(("The map's place", lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::{press, type_into};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn listing_needs_nothing_else() {
        let mut screen = Ortho::new();
        assert!(command(&screen).is_err());
        press(&mut screen.form, LIST, KeyCode::Enter);
        screen.changed(LIST);
        assert!(!screen.form.shown(MAP));
        assert_eq!(command(&screen).unwrap(), ["ortho", "--list-services"]);
    }

    #[test]
    fn a_map_and_a_service_make_a_command() {
        let mut screen = Ortho::new();
        type_into(&mut screen.form, MAP, "forest.omap");
        assert_eq!(command(&screen).unwrap(), ["ortho", "forest.omap"]);
        screen
            .form
            .set_choices(SERVICE, vec![BEST.into(), "it-emilia-romagna-2023".into()]);
        press(&mut screen.form, SERVICE, KeyCode::Right);
        type_into(&mut screen.form, PIXEL, "0.5");
        assert_eq!(
            command(&screen).unwrap()[2..],
            ["--service", "it-emilia-romagna-2023", "--pixel-size", "0.5"]
        );
    }

    #[test]
    fn a_map_with_no_place_says_so() {
        let mut screen = Ortho::new();
        type_into(&mut screen.form, MAP, "tests/data/shapes.xmap");
        screen.changed(MAP);
        screen.place.wait();
        assert!(matches!(screen.place, Loading::Failed(_)));
    }
}
