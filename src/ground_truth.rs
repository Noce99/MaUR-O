//! What a network is supposed to say about a rendered map, pixel by pixel.
//!
//! A generated map is the one map whose answer is known: it was not surveyed
//! and then labelled, it was drawn from a list of decisions, and the list is
//! still there when the image comes out. This module writes that list down in
//! the shape a model is scored against — one label per pixel of the image
//! [`crate::render`] drew, on the same pixel grid, out of the same geometry.
//!
//! # What the labels say
//!
//! For a map generated with [`crate::dataset::Settings::just_opaque_areas`],
//! every pixel of the image is one of the symbol set's opaque areas, or it is
//! the white frame around the map. So the answer for one pixel is:
//!
//! * which of the `on` opaque areas covers it — a one-hot vector of `on`
//!   numbers, all of them zero where nothing does;
//! * plus how the ground under it was turned: an area which fills with a
//!   pattern that turns is drawn at an angle of its own, and two pixels of
//!   the same symbol at different angles do not look alike.
//!
//! That is a tensor of `H` by `W` by `on + 2`, and it is what
//! [`GroundTruth::one_hot`] hands over. The angle is the last two channels,
//! its sine and then its cosine, because an angle is not a number a model can
//! be scored on as one: a whole turn brings a pattern back to where it
//! started, so 0.001 and 0.999 of a turn are a pattern a degree apart, and a
//! squared error on the angle itself calls them the two furthest apart
//! answers there are. The point on the unit circle has no such seam, and the
//! angle comes back out of it as `atan2(sin, cos)`.
//!
//! A symbol with no pattern to turn gets `(0, 0)` rather than a point on the
//! circle — the zero vector is no angle at all, which is what there is to say
//! about it — and so does the frame around the map. That leaves the two
//! channels able to say "no angle here" as well as which angle, and a
//! rotation loss can be masked by the length of the target vector.
//!
//! # What is on disk
//!
//! Not that tensor. A 1650 by 1650 image of a set with thirty opaque areas is
//! 345 MB of mostly zeros, which is a bad way to keep a dataset and a worse
//! way to read one. A file holds the same thing in the form it was decided
//! in: one class index per pixel and one angle per pixel, `MAUROGT2`:
//!
//! ```text
//! 0..8     b"MAUROGT2"      the format, the trailing digit its version
//! 8..12    u32              height, in pixels
//! 12..16   u32              width, in pixels
//! 16..20   u32              on, the number of one-hot channels
//! 20..32   zeros            room for what a later version needs
//! 32..     [u16; H * W]     the class of each pixel, row by row
//! ..       [f32; H * W]     the angle of each pixel, row by row
//! ```
//!
//! Every number is little-endian. A class is a place in the symbol list of
//! the dataset's `classes.json`, and [`BACKGROUND`] for a pixel no ground
//! cover reaches. An angle is a share of a whole turn in `[0, 1)`, and
//! [`NO_ROTATION`] where the symbol has no pattern to turn — the one angle
//! which is not one, and which the sine and the cosine come out of as the
//! zero vector.
//!
//! The angle is kept rather than its sine and cosine because it is what the
//! map was drawn at and it is half the size; the pair is worked out where the
//! tensor is. Six bytes a pixel rather than `4 * (on + 2)`, and the expansion
//! is a loop in whatever assembles a batch — where it costs one image's worth
//! of memory instead of a folder's worth of disk.
//!
//! [`GroundTruth::read`] is that loop's other half, and the reference for
//! anything reading these files from elsewhere.
//!
//! # Reading one
//!
//! What a [burn](https://burn.dev) `Dataset` returns for the n-th item of a
//! folder, given the image beside it:
//!
//! ```no_run
//! # use std::path::Path;
//! # use maur_o::ground_truth::GroundTruth;
//! let truth = GroundTruth::read(Path::new("dataset/gt/map_001.bin"))
//!     .expect("cannot read the labels");
//! let shape = [
//!     truth.height as usize,
//!     truth.width as usize,
//!     truth.channels(),
//! ];
//! # let _ = shape;
//! // Tensor::<B, 3>::from_floats(truth.one_hot().as_slice(), device).reshape(shape)
//! ```
//!
//! A model which predicts the class alone wants no one-hot at all: a
//! cross-entropy loss takes the class index, which is
//! [`GroundTruth::class_of`] as it comes out of the file — with
//! [`BACKGROUND`] mapped to a class of its own or masked out of the loss,
//! since it is the one label with no channel. The angle is then
//! [`GroundTruth::sin_cos`] on its own, over the pixels whose target is not
//! the zero vector.

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use tiny_skia::{FillRule, Mask, Transform};

use crate::geometry::to_painter_path;
use crate::map::{CoordList, ObjectKind};
use crate::renderer::to_skia_path;
use crate::symbol_kinds::Catalogue;
use crate::xml_reader::read_xml_map;

/// The eight bytes a ground truth file opens with. The digit is the version:
/// a file which starts with something else is not one of these.
pub const MAGIC: [u8; 8] = *b"MAUROGT2";

/// How many bytes come before the first pixel.
pub const HEADER_BYTES: usize = 32;

/// The class of a pixel no ground cover reaches — the white frame around the
/// map. Its one-hot vector is all zeros, so it is a class in the file and no
/// channel in the tensor.
pub const BACKGROUND: u16 = u16::MAX;

/// The angle of a pixel whose symbol has no pattern to turn, which is no
/// angle rather than an angle of nothing.
///
/// Outside the `[0, 1)` a real angle is written in, so the two cannot be
/// confused: a symbol which turns and came up at nought is `0.0`, and a
/// symbol with nothing to turn is this. It becomes the zero vector where the
/// sine and the cosine are worked out — see [`GroundTruth::sin_cos`].
pub const NO_ROTATION: f32 = -1.0;

/// One piece of ground as it was drawn, which is one region of the answer.
pub struct Ground {
    /// Which of the symbol set's opaque areas it was filled with: a place in
    /// [`crate::symbol_kinds::Catalogue::opaque_areas`], and the one-hot
    /// channel that symbol owns.
    pub class: usize,
    /// The angle the fill pattern was turned to, in radians, as the map file
    /// carries it — and `None` for a symbol with no pattern to turn, which
    /// has no angle rather than an angle of nothing.
    pub turn: Option<f64>,
    /// The shape it covers, in the coordinates a map file holds — mm on the
    /// paper.
    pub outline: CoordList,
}

/// The answer for one image, in the compact form a file holds.
pub struct GroundTruth {
    /// The height of the image these labels are for, in pixels.
    pub height: u32,
    /// Its width, in pixels.
    pub width: u32,
    /// How many one-hot channels there are: the number of opaque areas in the
    /// symbol set the dataset was generated from. The tensor has two more,
    /// for the angle.
    pub classes: usize,
    /// The class of each pixel, row by row, [`BACKGROUND`] where none.
    pub class_of: Vec<u16>,
    /// The angle of each pixel, row by row, as a share of a whole turn in
    /// `[0, 1)`, and [`NO_ROTATION`] where the symbol has no pattern to turn.
    ///
    /// The tensor carries this as its sine and its cosine, which is what a
    /// model can be scored on; this is the angle those come from.
    pub rotation: Vec<f32>,
}

impl GroundTruth {
    /// The labels for an image of `width` by `height` pixels showing
    /// `grounds`, drawn through `page_transform` — the one
    /// [`crate::render::Rendering::page_transform`] gives for that image.
    ///
    /// The pieces are laid down in the order they are given, as a painter
    /// lays them down, so a later one wins where two overlap. The cells of a
    /// generated map do not overlap: they are cut out of one square by shared
    /// boundaries, and the only pixels two of them could both claim are the
    /// ones the boundary itself runs through.
    ///
    /// Every pixel is one class or another with nothing in between, so the
    /// shapes are rasterized without anti-aliasing: along a boundary the
    /// image blends two symbols over a pixel or so, and the label there is
    /// whichever one covers the pixel's centre. There is no half a symbol to
    /// be right about.
    pub fn rasterize(
        grounds: &[Ground],
        classes: usize,
        width: u32,
        height: u32,
        page_transform: Transform,
    ) -> Result<GroundTruth, String> {
        let pixels = width as usize * height as usize;
        let mut truth = GroundTruth {
            height,
            width,
            classes,
            class_of: vec![BACKGROUND; pixels],
            // The frame around the map is no ground and so no angle either.
            rotation: vec![NO_ROTATION; pixels],
        };
        let mut mask = Mask::new(width, height)
            .ok_or_else(|| format!("cannot label an image of {width}x{height} pixels"))?;

        for ground in grounds {
            if ground.class >= classes {
                return Err(format!(
                    "the ground is drawn with symbol {} of {classes}",
                    ground.class
                ));
            }
            let Some(path) = to_skia_path(&to_painter_path(&ground.outline, false)) else {
                continue;
            };
            mask.clear();
            // Even-odd, as `QPainterPath` fills by default and as the
            // renderer fills a path object: an inner loop of the same shape
            // punches a hole rather than filling it twice over.
            mask.fill_path(&path, FillRule::EvenOdd, false, page_transform);

            let class = ground.class as u16;
            // A whole turn is no turn, and a file may carry either; the
            // labels only ever hold the fraction of a turn left over. A
            // symbol with no pattern to turn has no angle to fold at all.
            let rotation = match ground.turn {
                Some(turn) => (turn.rem_euclid(TAU) / TAU) as f32,
                None => NO_ROTATION,
            };
            for (at, &covered) in mask.data().iter().enumerate() {
                if covered != 0 {
                    truth.class_of[at] = class;
                    truth.rotation[at] = rotation;
                }
            }
        }
        Ok(truth)
    }

    /// Rebuilds the labels for a map [`crate::dataset::create_dataset`] wrote
    /// with [`crate::dataset::Settings::just_opaque_areas`] on, straight out
    /// of the map file itself rather than out of a `.bin`
    /// [`GroundTruth::write`] wrote beside it.
    ///
    /// A map like that *is* its own answer: every object on it is one cell,
    /// filled with one opaque area of the symbol set and turned to the angle
    /// its pattern was drawn at, and nothing else was drawn over it. So the
    /// grounds [`GroundTruth::rasterize`] wants are read straight back out of
    /// the objects rather than kept from when they were drawn — which is what
    /// lets a `gt/` folder be left out of a dataset and made up again on
    /// demand, at the cost of a map file to parse and rasterize instead of a
    /// `.bin` to read.
    ///
    /// `width` and `height` are the image this is to line up with, pixel for
    /// pixel — they do not come from the map file, which says nothing about
    /// how many pixels a meter of ground was drawn at. Neither does
    /// `resolution`: it is one of the settings a dataset's `classes.json`
    /// exists to remember.
    ///
    /// An object drawn with a symbol the catalogue does not carry as an
    /// opaque area — a line, a point, anything left over from a map not
    /// generated with `just_opaque_areas` — is skipped rather than
    /// complained about: it draws nothing a label can say, so it is treated
    /// as nothing having been drawn there, same as the frame around the map.
    ///
    /// Not bit for bit with a `.bin` [`GroundTruth::rasterize`] wrote at
    /// generation time: a map file keeps a rotation to six significant
    /// digits, same as Mapper does, rather than the exact angle it was
    /// turned to — close enough for a network trained on it, not close
    /// enough for `==`.
    pub fn from_map(
        path: &Path,
        width: u32,
        height: u32,
        resolution: f64,
    ) -> Result<GroundTruth, String> {
        let (mut map, _) = read_xml_map(path)?;
        map.resolve_references();
        let catalogue = Catalogue::of(&map);
        if catalogue.opaque_areas.is_empty() {
            return Err(format!(
                "{} holds no opaque area symbol, so there is no ground cover to label",
                path.display()
            ));
        }

        // Where a symbol's place in `Map::symbols` lands among the opaque
        // areas — an object names the first, and rasterizing wants the
        // second — and whether it turns, which is what tells a rotation
        // worth keeping from an angle nobody asked the symbol for.
        let channel_of: HashMap<usize, (usize, bool)> = catalogue
            .opaque_areas
            .iter()
            .enumerate()
            .map(|(channel, entry)| (entry.index, (channel, entry.turns)))
            .collect();

        let grounds: Vec<Ground> = map
            .objects
            .iter()
            .filter(|object| matches!(object.kind, ObjectKind::Path(_)))
            .filter_map(|object| {
                let &(class, turns) = channel_of.get(&object.symbol_index?)?;
                Some(Ground {
                    class,
                    turn: turns.then_some(object.rotation),
                    outline: object.coords.clone(),
                })
            })
            .collect();
        if grounds.is_empty() {
            return Err(format!(
                "{} draws no opaque area: there is no ground for a label to describe",
                path.display()
            ));
        }

        let page_transform = page_transform_for(map.scale_denominator, resolution, width, height);
        GroundTruth::rasterize(
            &grounds,
            catalogue.opaque_areas.len(),
            width,
            height,
            page_transform,
        )
    }

    /// How many channels the tensor has: one per opaque area of the symbol
    /// set, and two for the angle its pattern was turned to.
    pub fn channels(&self) -> usize {
        self.classes + 2
    }

    /// The angle of the pixel at `at` as the point on the unit circle the
    /// last two channels carry, and `(0.0, 0.0)` where there is no angle —
    /// the frame around the map, or a symbol with no pattern to turn.
    ///
    /// The angle comes back out of the pair as `sin.atan2(cos)`, in radians
    /// and over `(-pi, pi]` rather than the `[0, 1)` of a whole turn a file
    /// holds.
    pub fn sin_cos(&self, at: usize) -> (f32, f32) {
        let turn = self.rotation[at];
        if turn == NO_ROTATION {
            return (0.0, 0.0);
        }
        let angle = turn as f64 * TAU;
        let (sin, cos) = angle.sin_cos();
        (sin as f32, cos as f32)
    }

    /// The labels as the tensor a model is scored against: `H` by `W` by
    /// `channels`, row by row and pixel by pixel, each pixel a one-hot vector
    /// of its class followed by the sine and the cosine of its angle.
    ///
    /// This is the expanded form, and it is large — `4 * H * W * (on + 2)`
    /// bytes, which for a default dataset is a third of a gigabyte per image.
    /// It is meant to be built one image at a time, as a batch is assembled,
    /// and dropped again.
    pub fn one_hot(&self) -> Vec<f32> {
        let channels = self.channels();
        let mut tensor = vec![0.0; self.class_of.len() * channels];
        for (at, &class) in self.class_of.iter().enumerate() {
            let pixel = &mut tensor[at * channels..(at + 1) * channels];
            if class != BACKGROUND {
                pixel[class as usize] = 1.0;
            }
            let (sin, cos) = self.sin_cos(at);
            pixel[channels - 2] = sin;
            pixel[channels - 1] = cos;
        }
        tensor
    }

    /// Writes the labels to `path` in the `MAUROGT1` format this module
    /// describes.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let cannot = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
        let mut file = BufWriter::new(File::create(path).map_err(cannot)?);

        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(&MAGIC);
        header[8..12].copy_from_slice(&self.height.to_le_bytes());
        header[12..16].copy_from_slice(&self.width.to_le_bytes());
        header[16..20].copy_from_slice(&(self.classes as u32).to_le_bytes());
        file.write_all(&header).map_err(cannot)?;

        for &class in &self.class_of {
            file.write_all(&class.to_le_bytes()).map_err(cannot)?;
        }
        for &rotation in &self.rotation {
            file.write_all(&rotation.to_le_bytes()).map_err(cannot)?;
        }
        file.flush().map_err(cannot)
    }

    /// Reads back a file [`GroundTruth::write`] wrote.
    pub fn read(path: &Path) -> Result<GroundTruth, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        GroundTruth::from_bytes(&bytes).map_err(|message| format!("{}: {message}", path.display()))
    }

    /// What a labels file says it holds, without reading it: the height, the
    /// width and the number of one-hot channels, out of the thirty-two byte
    /// header alone.
    ///
    /// A whole file is sixteen megabytes at the size a dataset is generated
    /// at, and something which only wants to know whether a folder's labels
    /// agree with each other — and with the images beside them — has no
    /// business reading all of it, once per map, to find out.
    pub fn size(path: &Path) -> Result<(u32, u32, usize), String> {
        use std::io::Read;

        let mut header = [0u8; HEADER_BYTES];
        File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if header[..8] != MAGIC {
            return Err(format!(
                "{}: not a MAUROGT2 ground truth file",
                path.display()
            ));
        }
        let number = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        Ok((number(8), number(12), number(16) as usize))
    }

    /// Reads back the bytes [`GroundTruth::write`] wrote, whatever they were
    /// read out of.
    pub fn from_bytes(bytes: &[u8]) -> Result<GroundTruth, String> {
        if bytes.len() < HEADER_BYTES || bytes[..8] != MAGIC {
            return Err("not a MAUROGT2 ground truth file".to_string());
        }
        let number = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let (height, width, classes) = (number(8), number(12), number(16) as usize);
        let pixels = height as usize * width as usize;

        let wanted = HEADER_BYTES + pixels * 6;
        if bytes.len() != wanted {
            return Err(format!(
                "{}x{} pixels want {wanted} bytes, and the file holds {}",
                width,
                height,
                bytes.len()
            ));
        }

        let (classes_at, rotations_at) = (HEADER_BYTES, HEADER_BYTES + pixels * 2);
        let class_of = bytes[classes_at..rotations_at]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let rotation = bytes[rotations_at..]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        Ok(GroundTruth {
            height,
            width,
            classes,
            class_of,
            rotation,
        })
    }
}

/// The transform from a map coordinate — mm on the paper — to the pixel of a
/// `width` by `height` image it lines up with, at `resolution` pixels per
/// meter of ground and the map's own scale.
///
/// The same arithmetic [`crate::render::render_map_over`] uses for
/// [`crate::render::Extent::Ground`], run backwards from the pixel size an
/// image already came out as, rather than forwards from a ground rectangle
/// which is then rounded into one — which is what keeps a label's pixel grid
/// exactly the image's, rather than a pixel off from rounding the same square
/// twice.
fn page_transform_for(
    scale_denominator: i32,
    resolution: f64,
    width: u32,
    height: u32,
) -> Transform {
    let pixel_per_mm = resolution * scale_denominator as f64 / 1000.0;
    let half_mm = |pixels: u32| pixels as f64 / pixel_per_mm / 2.0;
    let translate = Transform::from_translate(half_mm(width) as f32, half_mm(height) as f32);
    let scale = Transform::from_scale(pixel_per_mm as f32, pixel_per_mm as f32);
    translate.post_concat(scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Coord;

    /// A square from `(left, top)` to `(right, bottom)`, in mm.
    fn square(left: f64, top: f64, right: f64, bottom: f64) -> CoordList {
        use crate::map::coord_flag;
        vec![
            Coord::new(left, top, 0),
            Coord::new(right, top, 0),
            Coord::new(right, bottom, 0),
            Coord::new(left, bottom, coord_flag::CLOSE_POINT),
        ]
    }

    /// Four pixels of ground: one mm of map is one pixel, the left half drawn
    /// with symbol 0 and turned a quarter turn, the right half with symbol 1,
    /// which has no pattern to turn.
    fn halves() -> GroundTruth {
        let grounds = [
            Ground {
                class: 0,
                turn: Some(TAU / 4.0),
                outline: square(0.0, 0.0, 2.0, 4.0),
            },
            Ground {
                class: 1,
                turn: None,
                outline: square(2.0, 0.0, 4.0, 4.0),
            },
        ];
        GroundTruth::rasterize(&grounds, 3, 4, 4, Transform::identity())
            .expect("the shapes are inside the image")
    }

    #[test]
    fn a_pixel_takes_the_class_and_the_rotation_of_the_ground_covering_it() {
        let truth = halves();
        assert_eq!(truth.class_of[..4], [0, 0, 1, 1]);
        assert_eq!(truth.rotation[..4], [0.25, 0.25, NO_ROTATION, NO_ROTATION]);
        // The same in every row: the two halves run the height of the image.
        assert_eq!(truth.class_of[12..], [0, 0, 1, 1]);
    }

    #[test]
    fn a_pixel_no_ground_reaches_is_background() {
        let grounds = [Ground {
            class: 2,
            turn: None,
            outline: square(0.0, 0.0, 2.0, 2.0),
        }];
        let truth = GroundTruth::rasterize(&grounds, 3, 4, 4, Transform::identity())
            .expect("the shape is inside the image");
        assert_eq!(truth.class_of[..4], [2, 2, BACKGROUND, BACKGROUND]);
        // Background is a class of the file and no channel of the tensor, and
        // it has no angle either.
        let tensor = truth.one_hot();
        assert_eq!(tensor[2 * 5..3 * 5], [0.0, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn one_hot_is_the_class_set_and_the_angle_as_a_sine_and_a_cosine() {
        let truth = halves();
        // Five channels: three symbols, then the sine and the cosine.
        assert_eq!(truth.channels(), 5);
        let tensor = truth.one_hot();
        assert_eq!(tensor.len(), 4 * 4 * 5);

        // A quarter turn: sine one, cosine nothing.
        let (sin, cos) = (tensor[3], tensor[4]);
        assert_eq!(&tensor[..3], [1.0, 0.0, 0.0]);
        assert!((sin - 1.0).abs() < 1e-6 && cos.abs() < 1e-6, "{sin}, {cos}");

        // And a symbol with no pattern to turn has no angle: the zero vector,
        // which is nowhere on the circle a real angle is a point of.
        assert_eq!(tensor[2 * 5..3 * 5], [0.0, 1.0, 0.0, 0.0, 0.0]);
    }

    /// The angle comes back out of the pair it was written as.
    #[test]
    fn a_sine_and_a_cosine_are_the_angle_they_were_made_from() {
        for eighths in 0..8 {
            let turn = eighths as f64 / 8.0;
            let grounds = [Ground {
                class: 0,
                turn: Some(turn * TAU),
                outline: square(0.0, 0.0, 4.0, 4.0),
            }];
            let truth = GroundTruth::rasterize(&grounds, 1, 4, 4, Transform::identity())
                .expect("the shape is inside the image");
            let (sin, cos) = truth.sin_cos(0);
            let back = (sin as f64).atan2(cos as f64).rem_euclid(TAU) / TAU;
            assert!((back - turn).abs() < 1e-6, "{turn} came back as {back}");
        }
    }

    #[test]
    fn what_is_written_is_what_is_read_back() {
        let truth = halves();
        let file = tempfile::NamedTempFile::new().expect("a temporary file");
        truth.write(file.path()).expect("cannot write the labels");

        let read = GroundTruth::read(file.path()).expect("cannot read the labels back");
        assert_eq!((read.height, read.width, read.classes), (4, 4, 3));
        assert_eq!(read.class_of, truth.class_of);
        assert_eq!(read.rotation, truth.rotation);
    }

    /// A label rebuilt straight from a map file is the same label
    /// [`crate::dataset::create_dataset`] rasterized and wrote to disk for
    /// it: the same classes, the same angles, pixel for pixel.
    #[test]
    fn from_map_rebuilds_the_label_a_dataset_wrote_to_disk() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let settings = crate::dataset::Settings {
            layout_size: 2,
            cell_size: 20,
            maps: 3,
            just_opaque_areas: true,
            resolution: 2.0,
            frame: 5.0,
            ..crate::dataset::Settings::default()
        };
        let summary = crate::dataset::create_dataset(
            Path::new("tests/data/turning_patterns.xmap"),
            dir.path(),
            &settings,
            |_, _| {},
        )
        .expect("the dataset generates");

        for generated in &summary.written {
            let on_disk = GroundTruth::read(generated.ground_truth.as_deref().expect("labelled"))
                .expect("the labels this generated");
            let rebuilt = GroundTruth::from_map(
                &generated.map,
                on_disk.width,
                on_disk.height,
                settings.resolution,
            )
            .unwrap_or_else(|e| panic!("{}: {e}", generated.map.display()));

            assert_eq!(
                rebuilt.class_of,
                on_disk.class_of,
                "{}",
                generated.map.display()
            );
            // Not bit for bit: a map file keeps a rotation to six
            // significant digits, same as Mapper does, where the `.bin`
            // holds the exact angle it was drawn from.
            for (at, (&got, &wanted)) in rebuilt.rotation.iter().zip(&on_disk.rotation).enumerate()
            {
                assert!(
                    (got - wanted).abs() < 1e-5,
                    "{} pixel {at}: {got} rebuilt, {wanted} on disk",
                    generated.map.display(),
                );
            }
        }
    }

    #[test]
    fn a_file_which_is_not_one_is_refused() {
        assert!(GroundTruth::from_bytes(b"not a tensor").is_err());
        let mut truncated = vec![0u8; HEADER_BYTES];
        truncated[..8].copy_from_slice(&MAGIC);
        truncated[8..12].copy_from_slice(&4u32.to_le_bytes());
        truncated[12..16].copy_from_slice(&4u32.to_le_bytes());
        assert!(GroundTruth::from_bytes(&truncated).is_err());
    }
}
