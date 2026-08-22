//! **Ma**p **U**tils in **R**ust for **O**rienteering.
//!
//! Everything here is built on one data model: an orienteering map, as
//! [OpenOrienteering Mapper](https://www.openorienteering.org/apps/mapper/)
//! writes it in an `.omap`/`.xmap` file. [`xml_reader`] parses a file into
//! [`map`] and [`xml_writer`] writes one back out; every other module is a
//! consumer of that model, and a new thing to do with a map is a new module
//! rather than a new parser.
//!
//! Rendering is the largest of those consumers: this crate draws a map to a
//! raster image, and measures how closely it does so. Everything is pure
//! Rust, with a dedicated crate for each concern — XML parsing, path stroking
//! and rasterization, font shaping — so a map can be drawn without Mapper
//! itself, Qt, or a graphical environment installed.
//!
//! # Beyond drawing
//!
//! [`ocd`] reads OCAD's own file format, so a map in it can be used like any
//! other. [`runnability`] answers a different question of the same model: not
//! what the map looks like, but how fast it is to run through, as a grid a
//! route can be searched over — and [`route`] searches it, for the way
//! between two controls that takes the least time. [`validate`] asks the
//! question a mapper asks: where does this map depart from the standard it
//! is drawn to? And [`dem`] shades a model of the ground, so that terrain
//! can be seen at all. [`course`] lays a course out over a map: where the
//! circles, the lines between them and the numbers beside them go. And
//! [`svg`] writes the same drawing the renderer makes out as vector paths
//! rather than pixels. [`stats`] counts what a map holds — its objects, the
//! symbols it uses, how far its lines run and how much ground it covers.
//!
//! Mapper is both the inspiration and the yardstick. Its rendering rules are
//! what this crate reproduces: the drawing order a map's colours define, the
//! way a line symbol's dashes and border are laid out, how an area's fill
//! pattern is clipped. And since "close enough" is not something a renderer
//! can be asked about in the abstract, Mapper's own command line renderer is
//! the ground truth a whole suite of maps is scored against, image by image.
//!
//! # Drawing a map
//!
//! [`render::render_map`] is the entire pipeline in one call — read the file,
//! build the renderables, rasterize them — and is what the `map_to_image`
//! tool is built on:
//!
//! ```no_run
//! use std::path::Path;
//! use maur_o::render::{render_map, save_pixmap, DEFAULT_FRAME, DEFAULT_RESOLUTION};
//!
//! let drawn = render_map(
//!     Path::new("maps/forest_sample.omap"),
//!     DEFAULT_RESOLUTION,
//!     DEFAULT_FRAME,
//! )
//! .unwrap_or_else(|e| panic!("cannot draw the map: {e}"));
//!
//! println!("{} x {} m at 1:{}", drawn.ground_width, drawn.ground_height, drawn.scale_denominator);
//! save_pixmap(&drawn.pixmap, Path::new("forest_sample.png")).expect("cannot write the image");
//! ```
//!
//! Underneath it, [`xml_reader`] parses a file into the [`map`] data model,
//! [`renderer`] turns that model's objects into filled and stroked paths —
//! leaning on [`geometry`], [`qbezier`] and [`text`] for the hard parts — and
//! `tiny-skia` puts them on a pixel buffer.
//!
//! # Measuring a renderer
//!
//! The rest of the crate exists to answer "how close is it?", and to tell a
//! real rendering bug from the noise two different rasterizers make when they
//! draw the same edge. [`all_symbols`] takes a symbol set apart into one test
//! map per symbol and [`xml_writer`] writes them back out; [`naming`] keeps a
//! benchmark archive's files in an order everything agrees on; [`differences`]
//! compares a run's images against the reference ones and classifies every
//! pixel that disagrees; and [`archive_info`], [`progress`] and [`report`] are
//! the small pieces the `create_benchmark` and `benchmark` tools are assembled
//! from.

// Every public item carries a doc comment; this keeps it that way.
#![warn(missing_docs)]

pub mod all_symbols;
pub mod archive_info;
pub mod course;
pub mod dem;
pub mod differences;
pub mod geometry;
pub mod map;
pub mod naming;
pub mod ocd;
pub mod ocd_crs;
pub mod progress;
pub mod qbezier;
pub mod render;
pub mod renderer;
pub mod report;
pub mod route;
pub mod runnability;
pub mod stats;
pub mod svg;
pub mod text;
pub mod validate;
pub mod xml_reader;
pub mod xml_writer;
