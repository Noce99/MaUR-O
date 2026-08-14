//! Renders OpenOrienteering Mapper maps to raster images.
//!
//! Ported from the C++/Qt `map_to_image` tool in the parent directory,
//! algorithm-for-algorithm rather than translated line-by-line.

pub mod map;
pub mod qbezier;
pub mod geometry;
pub mod xml_reader;
pub mod text;
pub mod renderer;
