//! `mauro terrain`'s screen: the map whose ground to download, and, once it
//! is picked, where it is on the earth and which elevation services cover
//! it — worked out offline, so that the service is picked from those alone.

use std::path::Path;

use ratatui::style::Stylize;
use ratatui::text::Line;

use maur_o::orthophoto::geographic_anchor;
use maur_o::terrain::TerrainService;
use maur_o::terrain_services::TERRAIN_SERVICES;

use super::{number, pair, Loading, MAP_EXTENSIONS};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

/// The service choice that leaves it to `mauro terrain`.
const BEST: &str = "the best for the map";

const MAP: usize = 0;
const OUTPUT: usize = 1;
const SERVICE: usize = 2;
const PIXEL: usize = 3;
const MARGIN: usize = 4;
const LIST: usize = 5;

/// Where the picked map is, and what covers it.
struct Place {
    /// Its latitude and longitude, where the file says.
    at: Option<(f64, f64)>,
    epsg: i32,
    /// The services whose box holds the map, best first (all of them,
    /// where the map's place in degrees is not known).
    services: Vec<&'static TerrainService>,
}

impl Place {
    fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let xml = if maur_o::ocd::is_ocd_file(&bytes) {
            maur_o::ocd::ocd_to_omap_xml(&bytes)?.xml
        } else {
            String::from_utf8(bytes).map_err(|e| e.to_string())?
        };
        let (map, _) = maur_o::xml_reader::read_xml_map_str(&xml)?;
        let epsg = map.georeferencing.as_ref().map_or(0, |g| g.epsg);
        if epsg == 0 {
            return Err(
                "The map is not georeferenced in a projected CRS with an EPSG code.".to_string(),
            );
        }
        let at = geographic_anchor(&xml).map(|a| (a.lat, a.lon));
        Ok(Self {
            at,
            epsg,
            services: TERRAIN_SERVICES
                .iter()
                .filter(|s| at.is_none_or(|(lat, lon)| s.covers(lat, lon)))
                .collect(),
        })
    }
}

pub struct Terrain {
    form: Form,
    place: Loading<Place>,
}

impl Terrain {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["terrain"],
            "Download",
            vec![
                Field::path("Map", Pick::Files(MAP_EXTENSIONS)).arg("map_file"),
                Field::text("Output")
                    .arg("output")
                    .hint("the map's name, with the service's id and .tif"),
                Field::choice("Service", &[BEST]).arg("service").help(
                    "The service to download from: the best one for where the map is (the \
                     first, best first, with elevations of all of it), or one of those whose \
                     area holds the map, listed beside once the map is picked.",
                ),
                Field::text("Pixel size")
                    .arg("pixel_size")
                    .unit("m per pixel")
                    .hint("the model's own"),
                Field::text("Margin").arg("margin").unit("m on the ground"),
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
        for field in [MAP, OUTPUT, SERVICE, PIXEL, MARGIN] {
            self.form.show(field, !listing);
        }
    }
}

impl Tool for Terrain {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::terrain::exit_meaning
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
            return Ok(vec!["terrain".into(), "--list-services".into()]);
        }
        let map = form.given(MAP).ok_or("Pick a map first (Enter on Map).")?;
        let mut args = vec!["terrain".to_string(), map.to_string()];
        form.push_option(&mut args, OUTPUT, "--output");
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
        Ok(args)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        if self.form.on(LIST) {
            return None;
        }
        let lines = self.place.status("No map picked yet.").unwrap_or_else(|| {
            let place = self.place.ready().expect("ready when there is no status");
            let mut lines = vec![
                pair(
                    "Where",
                    place
                        .at
                        .map_or("not given in degrees".to_string(), |(lat, lon)| {
                            format!("{lat:.5}, {lon:.5}")
                        }),
                ),
                pair("CRS", format!("EPSG:{}", place.epsg)),
                Line::default(),
            ];
            if place.services.is_empty() {
                lines.push(Line::from("No elevation service covers it.").red());
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
        let mut screen = Terrain::new();
        assert!(command(&screen).is_err());
        press(&mut screen.form, LIST, KeyCode::Enter);
        screen.changed(LIST);
        assert!(!screen.form.shown(MAP));
        assert_eq!(command(&screen).unwrap(), ["terrain", "--list-services"]);
    }

    #[test]
    fn a_map_and_a_service_make_a_command() {
        let mut screen = Terrain::new();
        type_into(&mut screen.form, MAP, "forest.omap");
        assert_eq!(command(&screen).unwrap(), ["terrain", "forest.omap"]);
        screen
            .form
            .set_choices(SERVICE, vec![BEST.into(), "it-tinitaly".into()]);
        press(&mut screen.form, SERVICE, KeyCode::Right);
        type_into(&mut screen.form, PIXEL, "2");
        type_into(&mut screen.form, OUTPUT, "ground.tif");
        assert_eq!(
            command(&screen).unwrap()[2..],
            [
                "--output",
                "ground.tif",
                "--service",
                "it-tinitaly",
                "--pixel-size",
                "2"
            ]
        );
        type_into(&mut screen.form, PIXEL, "0");
        assert!(command(&screen).is_err());
    }

    #[test]
    fn a_map_with_no_place_says_so() {
        let mut screen = Terrain::new();
        type_into(&mut screen.form, MAP, "tests/data/shapes.xmap");
        screen.changed(MAP);
        screen.place.wait();
        assert!(matches!(screen.place, Loading::Failed(_)));
    }
}
