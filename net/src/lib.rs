//! A U-Net trained to read a rendered orienteering map back into the symbols
//! it was drawn with.
//!
//! The renderer this sits beside turns a map file into a picture. This goes
//! the other way: given the picture, which of the symbol set's opaque areas
//! is each pixel, and at what angle was its fill pattern turned. It is the
//! same question a person answers by looking at a legend, and the training
//! data for it is not a survey anybody had to label — `generate_maps_dataset`
//! draws a map out of a list of decisions and writes the list down beside the
//! image, so the answers were known before the question existed.
//!
//! ```bash
//! # A few hundred maps, all ground cover, drawn and labelled.
//! cargo run --release --bin generate_maps_dataset -- \
//!     --just-opaque-areas -n 300 maps/ISOM_10k.omap dataset
//!
//! # And a network read off them.
//! cargo run --release -p maur-o-net --bin train -- dataset runs/first
//! ```
//!
//! Three pieces, and they are meant to be read in this order:
//!
//! * [`data`] turns the folder on disk into batches of tensors — which is
//!   mostly a story about cropping, because a map is 1650 pixels square and a
//!   U-Net is not;
//! * [`unet`] is the network, and what its output channels mean;
//! * [`training`] is what it is scored on and the loop which improves it.
//!
//! # The backend
//!
//! Picked at build time, since burn's is a type parameter rather than a
//! setting: `ndarray` by default, which is pure Rust and runs anywhere the
//! renderer does and is far too slow for a real run; `--features wgpu` for
//! any GPU with a Vulkan, Metal or DX12 driver; `--features cuda` for an
//! NVIDIA one. The `train` binary is built against whichever is on.

// Every public item carries a doc comment, as in the renderer beside it.
#![warn(missing_docs)]

pub mod data;
pub mod training;
pub mod unet;
