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
//! can be seen at all, while [`dem_geo_tiff_evaluator`] scores a predicted
//! one against a ground truth. [`course`] lays a course out over a map: where the
//! circles, the lines between them and the numbers beside them go. And
//! [`svg`] writes the same drawing the renderer makes out as vector paths
//! rather than pixels. [`stats`] counts what a map holds — its objects, the
//! symbols it uses, how far its lines run and how much ground it covers —
//! and [`snap`] says what is *at* a place on it, which is what a control
//! being placed needs to know.
//!
//! Mapper is both the inspiration and the yardstick. Its rendering rules are
//! what this crate reproduces: the drawing order a map's colours define, the
//! way a line symbol's dashes and border are laid out, how an area's fill
//! pattern is clipped. And since "close enough" is not something a renderer
//! can be asked about in the abstract, Mapper's own command line renderer is
//! the ground truth a whole suite of maps is scored against, image by image.
//!
//! It also goes the other way, from the world to a map: [`osm`] reads
//! OpenStreetMap data, [`osm_orienteering`] says what each feature looks like
//! on an orienteering map drawn to ISSprOM or ISOM, [`osm_style`] what it
//! looks like on openstreetmap.org's own map, and [`osm_convert`] makes a
//! whole Mapper map out of the data and either — with, where asked, bus and train lines over it, which
//! [`bus`] reads from the same data or from a GTFS feed, and [`bundle`]
//! lays out side by side where they share a street, and the areas of an
//! event's maps, which [`map_areas`] reads from a list. And it brings the
//! world *under* a map: [`orthophoto`] says which aerial photo of a map's
//! ground to ask which WMS for ([`orthophoto_services`] lists them, across
//! Europe), and [`image_template`] adds the
//! downloaded image to the map file as the template a mapper draws over.
//!
//! # From contours to elevation
//!
//! A second, largely independent pipeline reconstructs the shape of the
//! ground from a map's contour lines, following the algorithm specified in
//! `mds/Contours-to-Raster.md`. [`contour_symbols`] sorts a map's symbols
//! into the families the algorithm reads; [`step1_extract`] builds the
//! [`contour_raster`] and the per-contour evidence of [`gravity_model`] (with
//! [`contour_geometry`] for the curve work); [`step2_obvious_gravity`] and
//! [`step3_rain_drop`] decide which side of every contour is downhill;
//! [`step4_elevation`] gives each contour a height; and
//! [`step5_gravity_raster`] and [`step5_elevation_raster`] spread those
//! heights into a raster, which [`geotiff`] places on the earth. Its
//! parameters come from [`contours_to_raster_config`], and
//! [`contours_to_raster_svg`] draws every stage for inspection. The
//! `contours_to_raster` tool runs it on a file; [`dem_from_contours`] runs it
//! in memory, on a map already read. [`ocd_crs`] is what lets an OCAD map's
//! georeferencing survive the trip.
//!
//! # Drawing a map
//!
//! [`render::render_map`] is the entire pipeline in one call — read the file,
//! build the renderables, rasterize them — and is what the `map_to_image`
//! tool is built on. [`render::render_map_window`] does the same for one
//! rectangle of the map, given in map coordinates, instead of all of it:
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
pub mod bundle;
pub mod bus;
pub mod contour_geometry;
pub mod contour_raster;
pub mod contour_symbols;
pub mod contours_to_raster_config;
pub mod contours_to_raster_svg;
pub mod course;
pub mod dem;
pub mod dem_from_contours;
pub mod dem_geo_tiff_evaluator;
pub mod differences;
pub mod geometry;
pub mod geotiff;
pub mod gravity_model;
pub mod image_template;
pub mod map;
pub mod map_areas;
pub mod naming;
pub mod ocd;
pub mod ocd_crs;
pub mod orthophoto;
pub mod orthophoto_services;
pub mod osm;
pub mod osm_convert;
pub mod osm_orienteering;
pub mod osm_style;
pub mod progress;
pub mod qbezier;
pub mod render;
pub mod renderer;
pub mod report;
pub mod route;
pub mod runnability;
pub mod snap;
pub mod stats;
pub mod step1_extract;
pub mod step2_obvious_gravity;
pub mod step3_rain_drop;
pub mod step4_elevation;
pub mod step5_elevation_raster;
pub mod step5_gravity_raster;
pub mod svg;
pub mod text;
pub mod validate;
pub mod xml_reader;
pub mod xml_writer;
