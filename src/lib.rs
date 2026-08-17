//! **Ma**p **U**tils in **R**ust for **O**rienteering.
//!
//! Renders [OpenOrienteering Mapper](https://www.openorienteering.org/apps/mapper/)
//! `.omap`/`.xmap` files to raster images, and measures how closely it does
//! so. Everything is pure Rust, with a dedicated crate for each concern — XML
//! parsing, path stroking and rasterization, font shaping — so a map can be
//! drawn without Mapper itself, Qt, or a graphical environment installed.
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
//!
//! # Generating maps
//!
//! The maps which exist are never enough, and never put a symbol next to the
//! symbol which breaks it. [`dataset`] generates new ones out of nothing but
//! an existing symbol set, and is what the `generate_maps_dataset` tool is
//! built on: [`symbol_kinds`] sorts a symbol set into what its symbols are
//! for and works out what may be drawn over what and still be seen,
//! [`layout`] divides a square of ground into cells with wandering
//! boundaries — which are then filled with the set's opaque areas, lined
//! along their sides, covered with see-through areas and scattered with
//! point symbols — [`path_builder`] turns a shape in meters into the
//! coordinates a file holds, and [`random`] is where every choice any of
//! them makes comes from — seeded by hand, so that a generated map can be
//! generated again.
//!
//! A generated map is also the one map whose answer is known, which makes a
//! folder of them a training set: [`ground_truth`] writes down what each
//! pixel of a generated map's image is, as the tensor a model is scored
//! against, out of the very decisions the map was drawn from.

// Every public item carries a doc comment; this keeps it that way.
#![warn(missing_docs)]

pub mod all_symbols;
pub mod archive_info;
pub mod dataset;
pub mod differences;
pub mod geometry;
pub mod ground_truth;
pub mod layout;
pub mod map;
pub mod naming;
pub mod path_builder;
pub mod progress;
pub mod qbezier;
pub mod random;
pub mod render;
pub mod renderer;
pub mod report;
pub mod symbol_kinds;
pub mod text;
pub mod xml_reader;
pub mod xml_writer;
