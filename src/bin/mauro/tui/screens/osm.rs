//! `mauro osm`'s screen: where the data comes from (a file, or downloaded
//! for an area), the standard the map is drawn to, and, for
//! openstreetmap.org's look, its overlays. Beside it, how large the area is
//! and the scale the map comes out at.

use ratatui::style::Stylize;
use ratatui::text::Line;

use maur_o::osm::BBox;
use maur_o::osm_download::{parse_center, parse_size};

use super::{number, pair};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const FILE: &str = "a file";
const DOWNLOAD: &str = "downloaded";
const EVERYTHING: &str = "all of it";
const BOX: &str = "a box";
const AROUND: &str = "around a point";
const AUTO_DAY: &str = "auto";

const SOURCE: usize = 1;
const OSM_FILE: usize = 2;
const AREA: usize = 3;
const BBOX: usize = 4;
const CENTER: usize = 5;
const SIZE: usize = 6;
const SAVE_OSM: usize = 7;
const OVERPASS: usize = 8;
const MAX_AREA: usize = 9;
const OUTPUT: usize = 11;
const STANDARD: usize = 12;
const CARTO: usize = 13;
const DETAILS: usize = 14;
const BUS: usize = 15;
const BUS_EXCLUDE: usize = 16;
const TRAIN: usize = 17;
const GTFS: usize = 18;
const DAY: usize = 19;
const DATE: usize = 20;
const MAX_HEADWAY: usize = 21;
const BUNDLE_WIDTH: usize = 22;
const STOP_MERGE: usize = 23;
const MAPS: usize = 24;
const SINGLE_COLOR: usize = 25;
const DRINKING_WATER: usize = 26;
const LEGEND: usize = 27;

/// The scale of each `--details` level, as `mauro osm`'s documentation
/// tables it.
const DETAILS_SCALE: [u32; 5] = [24_000, 17_000, 10_000, 5_000, 2_500];

pub struct Osm {
    form: Form,
}

impl Osm {
    pub fn new() -> Self {
        let mut screen = Self {
            form: Form::new(
                &["osm"],
                "Convert",
                vec![
                    Field::heading("The data"),
                    Field::choice("Source", &[FILE, DOWNLOAD]).help(
                        "Where the OpenStreetMap data comes from: an .osm file (as \
                         openstreetmap.org's export, the Overpass API or osmium write one), or \
                         downloaded from the Overpass API for an area.",
                    ),
                    Field::path("OSM file", Pick::Files(&["osm"])).arg("osm_file"),
                    Field::choice("Area", &[EVERYTHING, BOX, AROUND]).help(
                        "What of the data to convert: all of the file, a box of longitudes \
                         and latitudes, or a square around a point. A download needs a box or \
                         a point.",
                    ),
                    Field::text("Box")
                        .arg("bbox")
                        .hint("min_lon,min_lat,max_lon,max_lat"),
                    Field::text("Point").arg("around").hint("lat,lon"),
                    Field::text("Size").arg("size"),
                    Field::text("Keep data")
                        .arg("save_osm")
                        .hint("area.osm, or nothing"),
                    Field::text("Overpass").arg("overpass_url"),
                    Field::text("Max area").arg("max_area").unit("km²"),
                    Field::heading("The map"),
                    Field::text("Output")
                        .arg("omap_file")
                        .hint("the OSM file's name, with .omap"),
                    Field::choice("Standard", &["issprom", "isom", "osm"]).arg("standard"),
                    Field::heading("openstreetmap.org's look"),
                    Field::choice("Details", &["0", "1", "2", "3", "4"]).arg("details"),
                    Field::text("Bus lines").arg("bus").hint("13,19,27 or all"),
                    Field::text("Leave out").arg("bus_exclude").hint("N1,N2"),
                    Field::text("Train lines")
                        .arg("train")
                        .hint("S1,S2A,MEX or all"),
                    Field::paths("GTFS feeds", Pick::FileOrFolder(&["zip"]))
                        .arg("gtfs")
                        .hint("feed.zip; another.zip"),
                    Field::choice("Day", &[AUTO_DAY, "weekday", "saturday", "sunday"]).arg("day"),
                    Field::text("Date").arg("date").hint("YYYYMMDD"),
                    Field::text("Max headway")
                        .arg("max_headway")
                        .unit("minutes"),
                    Field::text("Bundle width")
                        .arg("bundle_width")
                        .unit("carto pixels"),
                    Field::text("Stop merge").arg("stop_merge").unit("mm"),
                    Field::path("Event maps", Pick::Files(&["csv"])).arg("maps"),
                    Field::toggle("Single colour").arg("areas_single_color"),
                    Field::toggle("Drinking water").arg("drinking_water"),
                    Field::toggle("Legend").arg("legend"),
                ],
            ),
        };
        screen.update();
        screen
    }

    fn download(&self) -> bool {
        self.form.choice(SOURCE) == DOWNLOAD
    }

    fn carto(&self) -> bool {
        self.form.choice(STANDARD) == "osm"
    }

    /// Shows the fields the source, the area and the standard call for.
    fn update(&mut self) {
        let download = self.download();
        let area = self.form.choice(AREA).to_string();
        self.form.show(OSM_FILE, !download);
        self.form.show(BBOX, area == BOX);
        self.form.show(CENTER, area == AROUND);
        self.form.show(SIZE, area == AROUND);
        for field in [SAVE_OSM, OVERPASS, MAX_AREA] {
            self.form.show(field, download);
        }
        let carto = self.carto();
        for field in CARTO..=LEGEND {
            self.form.show(field, carto);
        }
        let gtfs = carto && !self.form.text(GTFS).is_empty();
        self.form.show(DAY, gtfs);
        self.form.show(DATE, gtfs);
        let maps = carto && !self.form.text(MAPS).is_empty();
        self.form.show(SINGLE_COLOR, maps);
    }

    /// The area asked for, where one is, in a form it can be measured in.
    fn area(&self) -> Result<Option<BBox>, String> {
        match self.form.choice(AREA) {
            BOX => {
                let text = self.form.text(BBOX);
                if text.is_empty() {
                    return Err("Give the box: min_lon,min_lat,max_lon,max_lat.".to_string());
                }
                BBox::parse(text).map(Some)
            }
            AROUND => {
                let text = self.form.text(CENTER);
                if text.is_empty() {
                    return Err("Give the point: lat,lon.".to_string());
                }
                let center = parse_center(text)?;
                let size = parse_size(self.form.text(SIZE))?;
                Ok(Some(BBox::square(center, size)))
            }
            _ => Ok(None),
        }
    }
}

impl Tool for Osm {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::osm::exit_meaning
    }

    fn changed(&mut self, _field: usize) {
        self.update();
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["osm".to_string()];
        let area = self.area()?;
        if self.download() {
            if area.is_none() {
                return Err("A download needs an area: a box or a point.".to_string());
            }
            args.push("--download".to_string());
            let output = form
                .given(OUTPUT)
                .ok_or("Name the map to write (Output): it is all a download is named by.")?;
            args.push(output.to_string());
            form.push_option(&mut args, SAVE_OSM, "--save-osm");
            form.push_option(&mut args, OVERPASS, "--overpass-url");
            number(form.text(MAX_AREA), "Max area")?;
            form.push_option(&mut args, MAX_AREA, "--max-area");
        } else {
            let file = form
                .given(OSM_FILE)
                .ok_or("Pick the .osm file (Enter on OSM file).")?;
            args.push(file.to_string());
            if let Some(output) = form.given(OUTPUT) {
                args.push(output.to_string());
            }
        }
        form.push_option(&mut args, BBOX, "--bbox");
        form.push_option(&mut args, CENTER, "--around");
        form.push_option(&mut args, SIZE, "--size");
        if form.choice(STANDARD) != "issprom" {
            args.extend(["--standard".to_string(), form.choice(STANDARD).to_string()]);
        }
        if self.carto() {
            if form.choice(DETAILS) != "3" {
                args.extend(["--details".to_string(), form.choice(DETAILS).to_string()]);
            }
            form.push_option(&mut args, BUS, "--bus");
            form.push_option(&mut args, BUS_EXCLUDE, "--bus-exclude");
            form.push_option(&mut args, TRAIN, "--train");
            for feed in form.text(GTFS).split(';').map(str::trim) {
                if !feed.is_empty() {
                    args.extend(["--gtfs".to_string(), feed.to_string()]);
                }
            }
            if form.shown(DAY) && form.choice(DAY) != AUTO_DAY {
                args.extend(["--day".to_string(), form.choice(DAY).to_string()]);
            }
            form.push_option(&mut args, DATE, "--date");
            form.push_option(&mut args, MAX_HEADWAY, "--max-headway");
            form.push_option(&mut args, BUNDLE_WIDTH, "--bundle-width");
            form.push_option(&mut args, STOP_MERGE, "--stop-merge");
            form.push_option(&mut args, MAPS, "--maps");
            form.push_flag(&mut args, SINGLE_COLOR, "--areas-single-color");
            form.push_flag(&mut args, DRINKING_WATER, "--drinking-water");
            form.push_flag(&mut args, LEGEND, "--legend");
        }
        Ok(args)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        let mut lines = Vec::new();
        match self.area() {
            Ok(Some(area)) => {
                let km2 = area.area_km2();
                let mut line = pair("Area", format!("{km2:.2} km²"));
                if self.download() {
                    let max = number(self.form.text(MAX_AREA), "Max area").unwrap_or(f64::NAN);
                    if km2 > max {
                        line = line.red();
                        lines.push(line);
                        lines.push(
                            Line::from(format!("over the {max} km² a download may be")).red(),
                        );
                    } else {
                        lines.push(line);
                    }
                } else {
                    lines.push(line);
                }
                let (lon, lat) = area.center();
                lines.push(pair("Centre", format!("{lat:.5}, {lon:.5}")));
            }
            Ok(None) => lines.push(pair("Area", "all the file holds".to_string())),
            Err(why) => lines.push(Line::from(why).dim()),
        }
        let scale = match self.form.choice(STANDARD) {
            "issprom" => 4_000,
            "isom" => 10_000,
            _ => {
                let level: usize = self.form.choice(DETAILS).parse().unwrap_or(3);
                DETAILS_SCALE[level.min(DETAILS_SCALE.len() - 1)]
            }
        };
        lines.push(pair("Scale", format!("1:{scale}")));
        Some(("The area", lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::{press, type_into};
    use ratatui::crossterm::event::KeyCode;

    fn set(screen: &mut Osm, field: usize, code: KeyCode) {
        press(&mut screen.form, field, code);
        screen.changed(field);
    }

    #[test]
    fn a_file_converts_as_it_is() {
        let mut screen = Osm::new();
        assert!(command(&screen).unwrap_err().contains(".osm file"));
        type_into(&mut screen.form, OSM_FILE, "tests/data/bologna_sample.osm");
        assert_eq!(
            command(&screen).unwrap(),
            ["osm", "tests/data/bologna_sample.osm"]
        );
        assert!(!screen.form.shown(DETAILS), "carto's options hidden");
    }

    #[test]
    fn a_download_is_of_an_area_and_names_only_the_map() {
        let mut screen = Osm::new();
        set(&mut screen, SOURCE, KeyCode::Right);
        assert!(!screen.form.shown(OSM_FILE));
        assert!(command(&screen).unwrap_err().contains("needs an area"));
        set(&mut screen, AREA, KeyCode::Left); // around a point
        type_into(&mut screen.form, CENTER, "44.4939,11.3431");
        type_into(&mut screen.form, SIZE, "2km");
        type_into(&mut screen.form, OUTPUT, "piazza.omap");
        assert_eq!(
            command(&screen).unwrap(),
            [
                "osm",
                "--download",
                "piazza.omap",
                "--around",
                "44.4939,11.3431",
                "--size",
                "2km"
            ]
        );
        let (_, lines) = screen.panel().unwrap();
        assert!(format!("{lines:?}").contains("4.00 km²"));
    }

    #[test]
    fn carto_overlays_go_with_the_osm_standard_only() {
        let mut screen = Osm::new();
        type_into(&mut screen.form, OSM_FILE, "city.osm");
        set(&mut screen, STANDARD, KeyCode::Left); // osm
        assert!(screen.form.shown(BUS));
        assert!(!screen.form.shown(DAY), "no GTFS feed, no day");
        type_into(&mut screen.form, BUS, "13,19");
        type_into(&mut screen.form, GTFS, "tper.zip; mex.zip");
        screen.changed(GTFS);
        set(&mut screen, DAY, KeyCode::Left); // sunday
        set(&mut screen, LEGEND, KeyCode::Enter);
        set(&mut screen, DETAILS, KeyCode::Right); // 4
        assert_eq!(
            command(&screen).unwrap()[2..],
            [
                "--standard",
                "osm",
                "--details",
                "4",
                "--bus",
                "13,19",
                "--gtfs",
                "tper.zip",
                "--gtfs",
                "mex.zip",
                "--day",
                "sunday",
                "--legend"
            ]
        );
        // Back to ISSprOM, the overlays are left off the command line.
        set(&mut screen, STANDARD, KeyCode::Right);
        assert_eq!(command(&screen).unwrap(), ["osm", "city.osm"]);
    }
}
