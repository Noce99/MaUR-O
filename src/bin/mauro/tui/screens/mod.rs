//! A screen per tool, each a [`Tool`](super::form::Tool) on the shared
//! form, and what they share: reading a file in the background, and the
//! summary of a map several of them show.

pub mod benchmark_create;
pub mod benchmark_run;
pub mod contours;
pub mod dem;
pub mod dem_eval;
pub mod ortho;
pub mod osm;
pub mod render;
pub mod terrain;
pub mod tiles_build;
pub mod tiles_query;
pub mod tiles_serve;

use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use maur_o::geometry::Rect as MapRect;

/// The suffixes of the maps `mauro` reads.
pub const MAP_EXTENSIONS: &[&str] = &["omap", "xmap", "ocd"];

/// Something read on a thread of its own, a large file taking a while, and
/// picked up between keys.
pub enum Loading<T> {
    None,
    Busy(Receiver<Result<T, String>>),
    Ready(T),
    Failed(String),
}

impl<T: Send + 'static> Loading<T> {
    /// Starts `read` on a thread of its own.
    pub fn start(read: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            sender.send(read()).ok();
        });
        Loading::Busy(receiver)
    }

    /// Picks up the result, once there is one.
    pub fn poll(&mut self) {
        if let Loading::Busy(receiver) = self {
            if let Ok(result) = receiver.try_recv() {
                *self = match result {
                    Ok(value) => Loading::Ready(value),
                    Err(e) => Loading::Failed(e),
                };
            }
        }
    }

    pub fn ready(&self) -> Option<&T> {
        match self {
            Loading::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// The lines a panel shows while there is no value to show.
    pub fn status(&self, nothing: &'static str) -> Option<Vec<Line<'static>>> {
        match self {
            Loading::None => Some(vec![Line::from(nothing).dim()]),
            Loading::Busy(_) => Some(vec![Line::from("Reading…").italic()]),
            Loading::Failed(e) => Some(vec![Line::from(e.clone()).red()]),
            Loading::Ready(_) => None,
        }
    }

    /// Waits for the result, for tests.
    #[cfg(test)]
    pub fn wait(&mut self) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while matches!(self, Loading::Busy(_)) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll();
        }
    }
}

/// What a map is, in a few numbers.
#[derive(Clone, Debug)]
pub struct MapSummary {
    pub scale: i32,
    pub objects: usize,
    pub symbols: usize,
    /// The EPSG code of the CRS it is placed in, where it is placed at all.
    pub epsg: Option<i32>,
    pub warnings: usize,
    /// What the map's objects cover as drawn, in mm on the paper.
    pub extent: MapRect,
}

impl MapSummary {
    /// Reads the map at `path` (Mapper's or OCAD's).
    pub fn read(path: &Path) -> Result<Self, String> {
        let (map, warnings) = maur_o::render::read_map_file(path)?;
        let stats = maur_o::stats::stats(&map);
        Ok(Self {
            scale: map.scale_denominator,
            objects: stats.object_count,
            symbols: stats.symbol_count,
            epsg: map
                .georeferencing
                .as_ref()
                .map(|g| g.epsg)
                .filter(|epsg| *epsg != 0),
            warnings: warnings.len(),
            extent: maur_o::render::drawn_extent(&map),
        })
    }

    /// The summary as panel lines.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let e = &self.extent;
        let mut lines = vec![
            pair("Scale", format!("1:{}", self.scale)),
            pair("Objects", self.objects.to_string()),
            pair("Symbols", format!("{} in use", self.symbols)),
            pair(
                "Placed",
                match self.epsg {
                    Some(epsg) => format!("EPSG:{epsg}"),
                    None => "not georeferenced".to_string(),
                },
            ),
            pair(
                "Extent",
                format!(
                    "{:.0},{:.0},{:.0},{:.0} mm",
                    e.left(),
                    e.top(),
                    e.right(),
                    e.bottom()
                ),
            ),
        ];
        if self.warnings > 0 {
            lines.push(pair("Warnings", self.warnings.to_string()).yellow());
        }
        lines
    }
}

/// A `label  value` line of a panel.
pub fn pair(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{label:<9}")).bold(),
        Span::raw(value),
    ])
}

/// The number field `name` holds, or what is wrong with it.
pub fn number(text: &str, name: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{name} is not a number: {text:?}"))
}
