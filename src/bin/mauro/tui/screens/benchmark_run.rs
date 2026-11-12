//! `mauro benchmark run`'s screen: the archive, where the results go, and
//! how differences are counted. Beside it, what is in the archive, read the
//! way the run itself reads it.

use std::path::Path;

use ratatui::style::Stylize;
use ratatui::text::Line;

use super::{number, pair, Loading};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const ARCHIVE: usize = 0;
const RESULTS: usize = 1;
const FILTER: usize = 2;
const NAMES_ONLY: usize = 3;
const DIFFERENCES: usize = 4;
const TOLERANCE: usize = 5;
const KEEP_ANTIALIASING: usize = 6;
const CROPS: usize = 7;
const CROP_SIZE: usize = 8;
const ZOOM: usize = 9;
const OVERVIEW: usize = 10;

/// What an archive holds.
struct Holds {
    maps: usize,
    expected: usize,
    /// Its `info.txt`'s resolution and frame, where it has one.
    drawn_at: Option<(f64, f64)>,
}

impl Holds {
    fn read(archive: &Path) -> Result<Self, String> {
        let contents = crate::benchmark_run::read_contents(archive)?;
        let info = maur_o::archive_info::read(archive, &contents.root)?;
        Ok(Self {
            maps: contents.maps.len(),
            expected: contents.expected.len(),
            drawn_at: info.map(|i| (i.resolution, i.frame)),
        })
    }
}

pub struct BenchmarkRun {
    form: Form,
    archive: Loading<Holds>,
}

impl BenchmarkRun {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["benchmark", "run"],
            "Run",
            vec![
                Field::path("Archive", Pick::Files(&["zip"])).arg("archive"),
                Field::path("Results", Pick::Folder).arg("results"),
                Field::text("Filter").arg("filter").hint("every map"),
                Field::toggle("Names only").arg("names_only"),
                Field::heading("Differences"),
                Field::text("Tolerance").arg("tolerance"),
                Field::toggle("Antialiasing").arg("keep_antialiasing"),
                Field::text("Crops").arg("crops"),
                Field::text("Crop size").arg("crop_size").unit("px"),
                Field::text("Zoom").arg("zoom").unit("px"),
                Field::text("Overview").arg("overview").unit("px"),
            ],
        );
        form.open_picker(ARCHIVE);
        Self {
            form,
            archive: Loading::None,
        }
    }
}

impl Tool for BenchmarkRun {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::benchmark_run::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        match field {
            ARCHIVE => {
                let archive = Path::new(self.form.text(ARCHIVE)).to_path_buf();
                self.archive = if archive.is_file() {
                    Loading::start(move || Holds::read(&archive))
                } else {
                    Loading::None
                };
            }
            NAMES_ONLY => {
                let rendering = !self.form.on(NAMES_ONLY);
                for field in DIFFERENCES..=OVERVIEW {
                    self.form.show(field, rendering);
                }
            }
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.archive.poll();
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let archive = form
            .given(ARCHIVE)
            .ok_or("Pick the archive (Enter on Archive).")?;
        let mut args = vec!["benchmark".into(), "run".into(), archive.to_string()];
        form.push_option(&mut args, RESULTS, "--results");
        form.push_option(&mut args, FILTER, "--filter");
        form.push_flag(&mut args, NAMES_ONLY, "--names-only");
        for (field, name) in [
            (TOLERANCE, "Tolerance"),
            (CROPS, "Crops"),
            (CROP_SIZE, "Crop size"),
            (ZOOM, "Zoom"),
            (OVERVIEW, "Overview"),
        ] {
            if form.shown(field) && number(form.text(field), name)? < 0.0 {
                return Err(format!("{name} cannot be negative."));
            }
        }
        form.push_option(&mut args, TOLERANCE, "--tolerance");
        form.push_flag(&mut args, KEEP_ANTIALIASING, "--keep-antialiasing");
        form.push_option(&mut args, CROPS, "--crops");
        form.push_option(&mut args, CROP_SIZE, "--crop-size");
        form.push_option(&mut args, ZOOM, "--zoom");
        form.push_option(&mut args, OVERVIEW, "--overview");
        Ok(args)
    }

    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        let lines = self
            .archive
            .status("No archive picked yet.")
            .unwrap_or_else(|| {
                let holds = self.archive.ready().expect("ready when there is no status");
                let mut lines = vec![
                    pair("Maps", holds.maps.to_string()),
                    pair("Expected", holds.expected.to_string()),
                ];
                if holds.maps != holds.expected {
                    lines.push(Line::from("Not one reference image per map.").yellow());
                }
                lines.push(match holds.drawn_at {
                    Some((resolution, frame)) => {
                        pair("Drawn at", format!("{resolution} px/m, frame {frame} m"))
                    }
                    None => Line::from("No info.txt: drawn at the defaults.").dim(),
                });
                lines
            });
        Some(("The archive", lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::{press, type_into};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn an_archive_makes_a_command_and_names_only_hides_the_rest() {
        let mut screen = BenchmarkRun::new();
        assert!(command(&screen).is_err());
        type_into(&mut screen.form, ARCHIVE, "suite.zip");
        assert_eq!(command(&screen).unwrap(), ["benchmark", "run", "suite.zip"]);
        type_into(&mut screen.form, TOLERANCE, "30");
        press(&mut screen.form, KEEP_ANTIALIASING, KeyCode::Enter);
        assert_eq!(
            command(&screen).unwrap()[3..],
            ["--tolerance", "30", "--keep-antialiasing"]
        );
        press(&mut screen.form, NAMES_ONLY, KeyCode::Enter);
        screen.changed(NAMES_ONLY);
        assert_eq!(command(&screen).unwrap()[3..], ["--names-only"]);
        type_into(&mut screen.form, FILTER, "isom");
        assert!(command(&screen).unwrap().ends_with(&[
            "--filter".into(),
            "isom".into(),
            "--names-only".into()
        ]));
    }

    #[test]
    fn a_file_that_is_not_an_archive_says_so() {
        let mut screen = BenchmarkRun::new();
        type_into(&mut screen.form, ARCHIVE, "tests/data/shapes.xmap");
        screen.changed(ARCHIVE);
        screen.archive.wait();
        assert!(matches!(screen.archive, Loading::Failed(_)));
    }
}
