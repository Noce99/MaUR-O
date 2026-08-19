//! Turning a grid of symbols back into the map it was read off.
//!
//! [`crate::ground_truth`] goes one way: a map file's areas are rasterized
//! onto a pixel grid, one class per pixel, and that grid is what a model is
//! scored against. This module goes the other way. Given the grid — one
//! opaque area symbol per cell, however it was arrived at — it works out the
//! regions those cells form and writes them as the path objects of an
//! `.omap`, so that an answer which was a stack of numbers becomes a map
//! somebody can open, draw on and render again.
//!
//! The flattening a network's output needs to become such a grid — an argmax
//! over the one-hot channels, an `atan2` over the two angle ones — is the
//! caller's business. What arrives here is [`SymbolGrid`], and it is already
//! two-dimensional.
//!
//! # From cells to a shape
//!
//! A region is a 4-connected run of cells with the same class, and its
//! boundary is a staircase: a closed loop of unit edges on the lattice of
//! cell corners. Written out as it stands, that staircase is what the map
//! would look like — every step of it a right angle, at the size of one cell.
//!
//! It is not written out as it stands. Each unit edge of the boundary gets
//! one bezier node, at its middle, and each lattice vertex between two nodes
//! becomes a doubled control point. Along a straight run the control point
//! sits on the line between its neighbours and the segment is a straight
//! line; at a corner the two coincident controls turn the right angle into a
//! quarter of a curve, half a cell wide. That is what takes the staircase off
//! a boundary which was never square to begin with.
//!
//! Most of those nodes then go again. A node whose two lattice vertices are
//! both straight — the boundary runs through it without turning — says
//! nothing the line through it does not, so a run of `k` collinear edges is
//! left with two nodes rather than `k`, half a cell in from each end corner.
//!
//! Both steps are one rule, which is how [`to_objects`] states it:
//!
//! > simplify the staircase, then put a node half a cell in from each end of
//! > every surviving segment — or one node at its middle, where the segment
//! > is no longer than a cell — join those two with a straight line, and join
//! > across each surviving vertex with a cubic whose two controls both sit on
//! > that vertex.
//!
//! # The tolerance
//!
//! Simplifying the staircase at a tolerance of nothing removes the collinear
//! vertices and no others, which is the two steps above exactly. That is the
//! default, and on a grid the size of a rendered map it is not nearly enough:
//! at the three pixels per meter a dataset is drawn at, a cell boundary which
//! wandered smoothly across the ground comes back as a staircase which turns
//! every pixel or two, and almost nothing about it is collinear. Tens of
//! thousands of nodes per map, and edges which are bumpy at the scale of a
//! third of a meter.
//!
//! A tolerance greater than nought runs [Douglas–Peucker] over the staircase
//! first, in cells: no vertex of the boundary ends up further than that from
//! the line drawn in its place. It is off by default because it is a decision
//! about how much of the answer to keep, and that is not this module's to
//! make. It and the angle two neighbouring cells may differ by are the two
//! fields of [`Simplify`], and a caller which reads a grid off a network hands
//! both of them something looser: see
//! [`crate::net::predict::PREDICTED_TOLERANCE`].
//!
//! [Douglas–Peucker]: https://en.wikipedia.org/wiki/Ramer%E2%80%93Douglas%E2%80%93Peucker_algorithm
//!
//! # Holes
//!
//! A region which surrounds another is written as one object of several
//! parts: the outer loop, then a part for each loop around a hole, which is
//! what the `HOLE_POINT` flag on the coordinate before it says. The fill rule
//! is even-odd — what [`crate::geometry::to_painter_path`] builds and what
//! `GroundTruth::rasterize` labels through — so a part inside another part
//! punches through rather than filling twice.
//!
//! # Writing the file
//!
//! [`write_map`] is the whole of it: the colours and symbols of a source map
//! copied out verbatim, as [`crate::xml_writer`] wants them, and the grid's
//! regions as the objects between them. It is the same four calls
//! [`crate::dataset::create_dataset`] makes, so a map written here and a map
//! written there are the same kind of file.
//!
//! ```no_run
//! # use std::path::Path;
//! use maur_o::dataset::Settings;
//! use maur_o::ground_truth::GroundTruth;
//! use maur_o::vectorize::{write_map, Placement, Simplify, SymbolGrid};
//!
//! let truth = GroundTruth::read(Path::new("dataset/gt/map_001.bin"))
//!     .expect("cannot read the labels");
//! let written = write_map(
//!     &SymbolGrid::from(&truth),
//!     Path::new("maps/ISOM_10k.omap"),
//!     Path::new("map_001_back.omap"),
//!     &Placement {
//!         ground: Settings::default().ground(),
//!         scale_denominator: 10000,
//!     },
//!     &Simplify::default(),
//! )
//! .expect("cannot write the map");
//! println!("{} objects, {} coordinates", written.objects, written.coords);
//! ```

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::path::Path;

use crate::geometry::Rect;
use crate::ground_truth::{GroundTruth, BACKGROUND, NO_ROTATION};
use crate::map::{Object, ObjectKind, PathObject, Point};
use crate::path_builder::PathBuilder;
use crate::symbol_kinds::{Catalogue, Entry};
use crate::xml_reader;
use crate::xml_writer::MapFile;

/// How far from a corner a node is placed, in cells.
///
/// Half a cell each way, so the two nodes of a one-cell edge are the same
/// node — its middle — and the curve which rounds a corner is exactly as wide
/// as the cells the corner came from.
const CORNER: f64 = 0.5;

/// How much of what a grid says to keep, which is the one decision this
/// module does not make for itself.
///
/// [`Simplify::default`] is the rule this module's documentation states, and
/// is what a grid of exact labels wants. A grid read off a network wants both
/// numbers loosened — see the fields.
#[derive(Clone, Copy, Debug)]
pub struct Simplify {
    /// How far a boundary may be moved to be rid of a node, in cells.
    ///
    /// Nought keeps every node the staircase asks for. On a grid the size of
    /// a rendered map that is tens of thousands of them, none of which was
    /// ever in the map the picture came from.
    pub tolerance: f64,
    /// How far apart two neighbouring cells' angles may be and still be one
    /// object's, as a share of a whole turn.
    ///
    /// A symbol is drawn at an angle of its own, so two patches of the same
    /// ground cover turned two ways are two objects rather than one turned
    /// the average of them. The comparison is between neighbours rather than
    /// across the region, so a field which drifts stays one region however
    /// far it drifts; what it separates is a jump.
    ///
    /// [`SAME_ANGLE`] is right for a grid of exact labels, where the angle of
    /// a cell is the one a file held. A network's angle field is continuous
    /// and noisy, and a threshold that tight would shatter every turning area
    /// into fragments: something like a twentieth of a turn is what to hand
    /// it. [`f32::INFINITY`] never splits on the angle at all.
    pub same_angle: f32,
}

impl Default for Simplify {
    fn default() -> Simplify {
        Simplify {
            tolerance: 0.0,
            same_angle: SAME_ANGLE,
        }
    }
}

/// One opaque area symbol per cell: a map as a grid, which is what a model
/// says about one and what [`crate::ground_truth`] writes down about one.
///
/// The classes are places in a symbol list — [`Catalogue::opaque_areas`], in
/// the order `classes.json` records — and [`BACKGROUND`] where no ground
/// cover reaches, which is the white frame around a generated map. Both
/// vectors run row by row, `width` of them to a row.
#[derive(Clone, Debug)]
pub struct SymbolGrid {
    /// How many cells the grid is across.
    pub width: usize,
    /// How many cells it is down.
    pub height: usize,
    /// The class of each cell, row by row, [`BACKGROUND`] where none.
    pub class: Vec<u16>,
    /// The angle of each cell, row by row, as a share of a whole turn in
    /// `[0, 1)`, and [`NO_ROTATION`] where there is no angle to give.
    pub rotation: Vec<f32>,
}

impl SymbolGrid {
    /// A grid of `width` by `height` cells with nothing on it: every cell
    /// [`BACKGROUND`], and no angle anywhere.
    pub fn new(width: usize, height: usize) -> SymbolGrid {
        SymbolGrid {
            width,
            height,
            class: vec![BACKGROUND; width * height],
            rotation: vec![NO_ROTATION; width * height],
        }
    }

    /// The class of the cell at `column` and `row`.
    pub fn class_at(&self, column: usize, row: usize) -> u16 {
        self.class[row * self.width + column]
    }

    /// Turns a class of `frame` into [`BACKGROUND`].
    ///
    /// A network counts the white frame as the last class rather than as a
    /// sentinel, since a cross-entropy has no room for `0xFFFF` — see
    /// `UNet::frame_class`. A grid read off one wants this before it is
    /// vectorized, or the frame comes back as a symbol which is not there.
    pub fn frame_is(&mut self, frame: u16) {
        for class in &mut self.class {
            if *class == frame {
                *class = BACKGROUND;
            }
        }
    }

    /// Whether the two vectors are as long as the grid says they are.
    fn checked(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "a grid of {}x{} cells is no grid at all",
                self.width, self.height
            ));
        }
        let cells = self.width * self.height;
        if self.class.len() != cells {
            return Err(format!(
                "{}x{} cells want {cells} classes, and the grid holds {}",
                self.width,
                self.height,
                self.class.len()
            ));
        }
        if self.rotation.len() != cells {
            return Err(format!(
                "{}x{} cells want {cells} angles, and the grid holds {}",
                self.width,
                self.height,
                self.rotation.len()
            ));
        }
        Ok(())
    }
}

impl From<&GroundTruth> for SymbolGrid {
    /// The labels of an image, cell for pixel. The two are the same thing in
    /// different words: one class and one angle for each square of a grid.
    fn from(truth: &GroundTruth) -> SymbolGrid {
        SymbolGrid {
            width: truth.width as usize,
            height: truth.height as usize,
            class: truth.class_of.clone(),
            rotation: truth.rotation.clone(),
        }
    }
}

/// Where a grid sits on the ground, and what scale the coordinates it becomes
/// are to be read at.
pub struct Placement {
    /// The ground the whole grid covers, in **meters** — as
    /// [`crate::render::Extent::Ground`] takes it, x to the right and y
    /// downwards. One cell is `ground.width() / grid.width` across.
    pub ground: Rect,
    /// The scale of the map being written: 10000 for a 1:10000 one. What
    /// relates those meters to the mm on the paper a file holds.
    pub scale_denominator: i32,
}

/// What [`write_map`] came to.
pub struct Written {
    /// How many path objects the grid's regions came to.
    pub objects: usize,
    /// How many coordinates those objects hold between them, control points
    /// included — which is the number to watch when choosing a tolerance.
    pub coords: usize,
    /// Complaints from reading the source map's symbol set.
    pub warnings: Vec<String>,
}

/// The path objects a grid's regions come to, in the order the grid holds
/// them.
///
/// `symbols` is the list the classes index: [`Catalogue::opaque_areas`], in
/// the very order the dataset's `classes.json` recorded, since a class is a
/// place in it and nothing else says which place.
///
/// [`Simplify`] says how much of the boundary to keep and what counts as one
/// object's angle; [`Simplify::default`] is the rule this module's
/// documentation states.
///
/// Regions never overlap — a cell has one class — so the objects may be drawn
/// in any order, and a region inside another's hole shows through rather than
/// being painted over.
pub fn to_objects(
    grid: &SymbolGrid,
    symbols: &[Entry],
    placement: &Placement,
    simplify: &Simplify,
) -> Result<Vec<Object>, String> {
    grid.checked()?;
    let tolerance = simplify.tolerance;
    if tolerance.is_nan() || tolerance < 0.0 {
        return Err(format!(
            "the tolerance cannot be less than nothing, and is {tolerance} cells"
        ));
    }
    if simplify.same_angle.is_nan() || simplify.same_angle < 0.0 {
        return Err(format!(
            "the angle two cells may differ by cannot be less than nothing, and is {}",
            simplify.same_angle
        ));
    }

    let regions = regions_of(grid, symbols.len(), simplify.same_angle)?;
    // Map coordinates are in mm on the paper; a grid is in cells, and the
    // ground between them is in meters.
    let mm_per_meter = 1000.0 / placement.scale_denominator as f64;
    let cell = (
        placement.ground.width() / grid.width as f64,
        placement.ground.height() / grid.height as f64,
    );
    let corner = |at: (f64, f64)| {
        Point::new(
            placement.ground.left() + at.0 * cell.0,
            placement.ground.top() + at.1 * cell.1,
        )
    };

    let mut objects = Vec::with_capacity(regions.regions.len());
    for region in &regions.regions {
        let entry = &symbols[region.class];
        let mut loops = trace_loops(grid, &regions.region_of, region.id, &region.cells);
        if loops.is_empty() {
            continue;
        }
        // The outer loop first, as a file which somebody opens would have it.
        // The fill is even-odd, so this is a courtesy rather than a rule.
        loops.sort_by(|a, b| {
            area_of(b)
                .abs()
                .partial_cmp(&area_of(a).abs())
                .expect("lattice areas are finite")
        });

        let mut builder = PathBuilder::new(mm_per_meter);
        for boundary in &loops {
            let simplified = simplify_boundary(boundary, tolerance);
            let (start, steps) = nodes_of(&simplified);
            builder.move_to(corner(start));
            for step in steps {
                match step {
                    Step::Line(to) => builder.line_to(corner(to)),
                    Step::Curve(control, to) => {
                        let control = corner(control);
                        builder.curve_to(control, control, corner(to));
                    }
                }
            }
            builder.close();
        }

        let mut object = Object::new(ObjectKind::Path(PathObject::default()));
        object.coords = builder.coords;
        object.symbol_index = Some(entry.index);
        object.symbol_id = entry.id;
        // Only where the symbol has a pattern to turn. A file carries the
        // angle twice, once on the object and once on its `<pattern>`, as
        // `dataset::Generator::area_object` writes it.
        if entry.turns {
            if let Some(turn) = region.turn {
                object.rotation = turn;
                if let ObjectKind::Path(path) = &mut object.kind {
                    path.pattern_rotation = turn;
                }
            }
        }
        objects.push(object);
    }
    Ok(objects)
}

/// Writes the grid as a map file drawn with the symbol set of `source`.
///
/// The colours and the symbols go out as the source file's own bytes, whole
/// and in order — the colour table is the drawing order of a map and a symbol
/// names a colour by its place in it — and the objects between them are the
/// grid's regions. See [`crate::xml_reader::Fragments`] for why the symbols
/// are copied rather than written back out of the parsed model.
pub fn write_map(
    grid: &SymbolGrid,
    source: &Path,
    into: &Path,
    placement: &Placement,
    simplify: &Simplify,
) -> Result<Written, String> {
    let (mut map, warnings) = xml_reader::read_xml_map(source)?;
    map.resolve_references();
    let fragments = xml_reader::read_fragments(source)?;

    let catalogue = Catalogue::of(&map);
    if catalogue.opaque_areas.is_empty() {
        return Err(format!(
            "{} holds no opaque area symbol, so a class of a grid names nothing",
            source.display()
        ));
    }

    let objects = to_objects(grid, &catalogue.opaque_areas, placement, simplify)?;
    let coords = objects.iter().map(|object| object.coords.len()).sum();
    MapFile {
        scale_denominator: placement.scale_denominator,
        colors: &fragments.colors,
        symbols: &fragments.symbols,
        objects: &objects,
        // An angle is written only where the symbol has a pattern to turn,
        // so a rotation written here is a rotation something reads.
        rotatable: true,
    }
    .write(into)?;

    Ok(Written {
        objects: objects.len(),
        coords,
        warnings,
    })
}

// -- regions -----------------------------------------------------------------

/// A cell of the grid which belongs to no region: the white frame, and every
/// cell of a region still to be found.
const NO_REGION: u32 = u32::MAX;

/// The default of [`Simplify::same_angle`]: a hundredth of a whole turn.
///
/// Far below the smallest jump between two angles drawn at random, and far
/// above the disagreement inside one object of a grid of exact labels.
pub const SAME_ANGLE: f32 = 0.01;

/// One 4-connected run of cells with the same class.
struct Region {
    /// Which region this is, as [`Regions::region_of`] labels its cells.
    id: u32,
    /// A place in the symbol list.
    class: usize,
    /// The angle its fill pattern is turned to, in radians — the circular
    /// mean of its cells' — and `None` where none of them had one.
    turn: Option<f64>,
    /// The cells it is made of, in row-major order: the region's own boundary
    /// is traced from these rather than by looking over the whole grid for
    /// them, which is what keeps [`to_objects`] linear in the grid however
    /// many regions the grid falls into. A network part way through learning
    /// gives hundreds of thousands of them.
    cells: Vec<usize>,
}

/// The regions of a grid, and which of them each cell belongs to.
struct Regions {
    regions: Vec<Region>,
    /// One label per cell, [`NO_REGION`] for a cell of the frame.
    region_of: Vec<u32>,
}

/// Divides the grid into 4-connected runs of cells with the same class and,
/// to within `same_angle`, the same angle.
///
/// Diagonal touching is not connection: two cells of one symbol meeting at a
/// corner and nowhere else are two regions, which is how a rasterized shape
/// and its neighbour part company, and what the even-odd fill of each of them
/// then means.
fn regions_of(grid: &SymbolGrid, symbols: usize, same_angle: f32) -> Result<Regions, String> {
    let (width, height) = (grid.width, grid.height);
    let mut region_of = vec![NO_REGION; width * height];
    let mut regions = Vec::new();
    let mut stack = Vec::new();

    for start in 0..width * height {
        let class = grid.class[start];
        if class == BACKGROUND || region_of[start] != NO_REGION {
            continue;
        }
        if class as usize >= symbols {
            return Err(format!(
                "a cell is drawn with symbol {class} of {symbols}: a class is a place in the \
                 symbol list, and the frame is the background rather than a symbol past its end",
            ));
        }

        let id = regions.len() as u32;
        let mut cells = Vec::new();
        // The angle of a region is the circular mean of its cells', taken as
        // points on the unit circle: an angle has a seam where a whole turn
        // brings it back, and averaging across that seam is how a region
        // which came out at a hair under a turn and a hair over it ends up
        // pointing the other way.
        let (mut sin, mut cos, mut angled) = (0.0f64, 0.0f64, 0usize);
        stack.push(start);
        region_of[start] = id;
        while let Some(at) = stack.pop() {
            cells.push(at);
            let turn = grid.rotation[at];
            if turn != NO_ROTATION {
                let (s, c) = (turn as f64 * TAU).sin_cos();
                sin += s;
                cos += c;
                angled += 1;
            }
            let (column, row) = (at % width, at / width);
            let mut visit = |column: usize, row: usize, stack: &mut Vec<usize>| {
                let next = row * width + column;
                if grid.class[next] == class
                    && region_of[next] == NO_REGION
                    && angles_agree(turn, grid.rotation[next], same_angle)
                {
                    region_of[next] = id;
                    stack.push(next);
                }
            };
            if column > 0 {
                visit(column - 1, row, &mut stack);
            }
            if column + 1 < width {
                visit(column + 1, row, &mut stack);
            }
            if row > 0 {
                visit(column, row - 1, &mut stack);
            }
            if row + 1 < height {
                visit(column, row + 1, &mut stack);
            }
        }

        // A resultant of no length is a region whose cells point every way at
        // once, which is no angle rather than an angle of nothing.
        let turn = (angled > 0 && sin.hypot(cos) > 1e-9).then(|| sin.atan2(cos).rem_euclid(TAU));
        // The flood fill reaches the cells in whatever order the stack came
        // to them; row-major is the order `trace_loops` wants them in, since
        // which unit edge a loop starts at is the first one a scan of the grid
        // would have come to.
        cells.sort_unstable();
        regions.push(Region {
            id,
            class: class as usize,
            turn,
            cells,
        });
    }

    Ok(Regions { regions, region_of })
}

/// Whether two neighbouring cells were turned the same way: both of them by
/// no angle at all, or both by angles no further apart than `within` the
/// short way round the circle.
fn angles_agree(one: f32, other: f32, within: f32) -> bool {
    if one == NO_ROTATION || other == NO_ROTATION {
        return one == other;
    }
    let apart = (one - other).abs().rem_euclid(1.0);
    apart.min(1.0 - apart) <= within
}

// -- boundaries --------------------------------------------------------------

/// A corner of a cell: a point of the lattice the grid's cells sit between,
/// which runs from `(0, 0)` to `(width, height)`.
type Vertex = (i32, i32);

/// Every closed boundary loop of one region: the outer one, and one around
/// each hole.
///
/// Each side of each cell whose neighbour across it is not the region's
/// becomes one directed unit edge, turned so the region lies to its right —
/// which comes out clockwise around the region and anticlockwise around a
/// hole, y running downwards as map coordinates do.
///
/// Where two cells of the region meet at a corner and nowhere near it, a
/// lattice vertex has two edges leaving it. The region is 4-connected, so the
/// two cells are joined by some longer way round and the corner is a pinch
/// rather than a parting: the traversal takes the sharpest left turn there,
/// which carries the boundary through the pinch and leaves what the pinch
/// closed off as a hole of its own.
///
/// `cells` is the region's own cells, in row-major order — [`Region::cells`].
/// Looking for them over the whole grid instead would cost the grid once per
/// region, and a grid which falls into as many regions as it has cells is
/// then quadratic in its own size: an hour and more on the 1650-pixel picture
/// a part-trained network gives, which is what that reads as from outside.
fn trace_loops(grid: &SymbolGrid, region_of: &[u32], id: u32, cells: &[usize]) -> Vec<Vec<Vertex>> {
    let (width, height) = (grid.width as i32, grid.height as i32);
    let inside = |column: i32, row: i32| {
        column >= 0
            && row >= 0
            && column < width
            && row < height
            && region_of[(row * width + column) as usize] == id
    };

    // The edges, and which of them leave each vertex.
    let mut edges: Vec<(Vertex, Vertex)> = Vec::new();
    let mut leaving: HashMap<Vertex, Vec<usize>> = HashMap::new();
    let edge = |from: Vertex,
                to: Vertex,
                edges: &mut Vec<(Vertex, Vertex)>,
                leaving: &mut HashMap<Vertex, Vec<usize>>| {
        leaving.entry(from).or_default().push(edges.len());
        edges.push((from, to));
    };
    for &at in cells {
        let (c, r) = ((at % grid.width) as i32, (at / grid.width) as i32);
        if !inside(c, r - 1) {
            edge((c, r), (c + 1, r), &mut edges, &mut leaving);
        }
        if !inside(c + 1, r) {
            edge((c + 1, r), (c + 1, r + 1), &mut edges, &mut leaving);
        }
        if !inside(c, r + 1) {
            edge((c + 1, r + 1), (c, r + 1), &mut edges, &mut leaving);
        }
        if !inside(c - 1, r) {
            edge((c, r + 1), (c, r), &mut edges, &mut leaving);
        }
    }

    let mut used = vec![false; edges.len()];
    let mut loops = Vec::new();
    for first in 0..edges.len() {
        if used[first] {
            continue;
        }
        let mut boundary = Vec::new();
        let mut at = first;
        loop {
            used[at] = true;
            let (from, to) = edges[at];
            boundary.push(from);
            let heading = (to.0 - from.0, to.1 - from.1);
            let Some(&next) = leaving
                .get(&to)
                .into_iter()
                .flatten()
                .filter(|&&candidate| !used[candidate])
                .min_by_key(|&&candidate| {
                    let (from, to) = edges[candidate];
                    turn_rank(heading, (to.0 - from.0, to.1 - from.1))
                })
            else {
                break;
            };
            at = next;
        }
        if boundary.len() >= 3 {
            loops.push(boundary);
        }
    }
    loops
}

/// How far anticlockwise `next` turns from `heading`, as something to sort
/// by: nought for the sharpest left turn, three for turning back on itself.
///
/// Anticlockwise as it is seen, with y downwards: left of the way something
/// is heading is where the smaller y is.
fn turn_rank(heading: Vertex, next: Vertex) -> u8 {
    let left = (heading.1, -heading.0);
    let right = (-heading.1, heading.0);
    match next {
        n if n == left => 0,
        n if n == heading => 1,
        n if n == right => 2,
        _ => 3,
    }
}

/// Twice the signed area a loop encloses, by the shoelace formula. Only its
/// size is used here, to put the outer loop of a region first.
fn area_of(boundary: &[Vertex]) -> f64 {
    let mut sum = 0i64;
    for (at, &(x, y)) in boundary.iter().enumerate() {
        let (nx, ny) = boundary[(at + 1) % boundary.len()];
        sum += x as i64 * ny as i64 - nx as i64 * y as i64;
    }
    sum as f64
}

// -- simplification ----------------------------------------------------------

/// The vertices of a boundary worth keeping.
///
/// Always the corners alone: a vertex the boundary runs straight through says
/// nothing the line through it does not, and dropping it is what leaves a run
/// of collinear edges with two nodes rather than one per edge.
///
/// Then, where `tolerance` is more than nothing, Douglas–Peucker over what is
/// left, in cells.
fn simplify_boundary(boundary: &[Vertex], tolerance: f64) -> Vec<(f64, f64)> {
    let corners: Vec<(f64, f64)> = corners_of(boundary)
        .into_iter()
        .map(|(x, y)| (x as f64, y as f64))
        .collect();
    if tolerance <= 0.0 || corners.len() < 4 {
        return corners;
    }
    let reduced = peucker_loop(&corners, tolerance);
    // A tolerance which swallows a whole region leaves nothing to draw; the
    // shape it came from is a better answer than no shape.
    if reduced.len() >= 3 {
        reduced
    } else {
        corners
    }
}

/// The vertices at which a staircase loop turns, which is all of them but the
/// ones it runs straight through.
fn corners_of(boundary: &[Vertex]) -> Vec<Vertex> {
    let count = boundary.len();
    (0..count)
        .filter(|&at| {
            let before = boundary[(at + count - 1) % count];
            let here = boundary[at];
            let after = boundary[(at + 1) % count];
            let (ax, ay) = (here.0 - before.0, here.1 - before.1);
            let (bx, by) = (after.0 - here.0, after.1 - here.1);
            ax * by - ay * bx != 0
        })
        .map(|at| boundary[at])
        .collect()
}

/// Douglas–Peucker over a closed loop.
///
/// The algorithm simplifies a line between two ends, and a loop has none, so
/// it is cut in two: at the first vertex, and at whichever is furthest from
/// it. Two halves that far apart are two lines, and each is simplified as
/// one.
fn peucker_loop(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    let count = points.len();
    let far = (1..count)
        .max_by(|&a, &b| {
            distance(points[a], points[0])
                .partial_cmp(&distance(points[b], points[0]))
                .expect("lattice distances are finite")
        })
        .expect("a loop of at least four vertices");

    let mut keep = vec![false; count];
    keep[0] = true;
    keep[far] = true;
    peucker(points, 0, far, tolerance, &mut keep);
    // The second half runs past the end and back to the first vertex, which
    // is why the ends are indices into a list one longer than the loop.
    let mut wrapped: Vec<(f64, f64)> = points.to_vec();
    wrapped.push(points[0]);
    let mut wrapped_keep = keep.clone();
    wrapped_keep.push(true);
    peucker(&wrapped, far, count, tolerance, &mut wrapped_keep);

    (0..count)
        .filter(|&at| keep[at] || wrapped_keep[at])
        .map(|at| points[at])
        .collect()
}

/// Marks the vertices of `points[from..=to]` which are further than
/// `tolerance` from the line the rest would be replaced by.
fn peucker(points: &[(f64, f64)], from: usize, to: usize, tolerance: f64, keep: &mut [bool]) {
    if to <= from + 1 {
        return;
    }
    let (mut worst, mut at) = (0.0, from);
    for candidate in from + 1..to {
        let away = off_line(points[candidate], points[from], points[to]);
        if away > worst {
            worst = away;
            at = candidate;
        }
    }
    // Strictly further, so that a tolerance of nothing keeps nothing which
    // lies on the line: that is what makes the default the two steps exactly.
    if worst > tolerance {
        keep[at] = true;
        peucker(points, from, at, tolerance, keep);
        peucker(points, at, to, tolerance, keep);
    }
}

/// How far `point` is from the line through `from` and `to`.
fn off_line(point: (f64, f64), from: (f64, f64), to: (f64, f64)) -> f64 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    if length < f64::EPSILON {
        return distance(point, from);
    }
    ((point.0 - from.0) * dy - (point.1 - from.1) * dx).abs() / length
}

/// How far apart two points are.
fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

// -- nodes -------------------------------------------------------------------

/// One segment of the path a boundary becomes, after the node it starts from.
enum Step {
    /// A straight line to a node.
    Line((f64, f64)),
    /// A cubic to a node, both of whose controls sit on the corner it turns
    /// around.
    Curve((f64, f64), (f64, f64)),
}

/// The nodes a simplified boundary comes to: where the path starts, and the
/// segments which take it round and back.
///
/// Each segment of the boundary gets a node half a cell in from each of its
/// ends — one node at its middle, where it is no longer than a cell and the
/// two would meet — with a straight line between them. Each vertex between
/// two segments gets a cubic whose two controls both sit on it, which is what
/// rounds the corner.
fn nodes_of(boundary: &[(f64, f64)]) -> ((f64, f64), Vec<Step>) {
    let count = boundary.len();
    let segments: Vec<Vec<(f64, f64)>> = (0..count)
        .map(|at| {
            let from = boundary[at];
            let to = boundary[(at + 1) % count];
            let (dx, dy) = (to.0 - from.0, to.1 - from.1);
            let length = dx.hypot(dy);
            if length > 2.0 * CORNER {
                let (ux, uy) = (dx / length, dy / length);
                vec![
                    (from.0 + ux * CORNER, from.1 + uy * CORNER),
                    (to.0 - ux * CORNER, to.1 - uy * CORNER),
                ]
            } else {
                vec![((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0)]
            }
        })
        .collect();

    let start = segments[0][0];
    let mut steps = Vec::with_capacity(2 * count);
    for at in 0..count {
        if let [_, end] = segments[at][..] {
            steps.push(Step::Line(end));
        }
        // Round the corner this segment ends at, into the next one. The last
        // of these curves back to where the path started.
        steps.push(Step::Curve(
            boundary[(at + 1) % count],
            segments[(at + 1) % count][0],
        ));
    }
    (start, steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::coord_flag;

    /// A symbol list of `count` entries, none of which has a pattern to turn
    /// unless it is the first.
    fn symbols(count: usize) -> Vec<Entry> {
        (0..count)
            .map(|at| Entry {
                index: at,
                id: at as i32,
                code: format!("{at}"),
                name: format!("symbol {at}"),
                turns: at == 0,
            })
            .collect()
    }

    /// A grid whose cells are given by a picture: `.` is the frame and a
    /// digit is that class.
    fn grid(rows: &[&str]) -> SymbolGrid {
        let width = rows[0].len();
        let mut grid = SymbolGrid::new(width, rows.len());
        for (row, line) in rows.iter().enumerate() {
            assert_eq!(line.len(), width, "the rows are not one length");
            for (column, cell) in line.chars().enumerate() {
                if cell != '.' {
                    grid.class[row * width + column] =
                        cell.to_digit(10).expect("a digit or a dot") as u16;
                }
            }
        }
        grid
    }

    /// One cell of the grid is one meter of ground on a 1:1000 map, which
    /// makes a cell exactly one mm on the paper and a coordinate exactly a
    /// thousand native units.
    fn placement(width: usize, height: usize) -> Placement {
        Placement {
            ground: Rect::from_ltrb(0.0, 0.0, width as f64, height as f64),
            scale_denominator: 1000,
        }
    }

    /// The nodes of an object, in cells — the coordinates which are nodes
    /// rather than control points, as the flags say.
    fn nodes(object: &Object) -> Vec<(f64, f64)> {
        let mut nodes = Vec::new();
        let mut skip = 0;
        for coord in &object.coords {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if coord.is_curve_start() {
                skip = 2;
            }
            // The last coordinate of a part repeats the node it started
            // from, which is a node already counted.
            if !coord.is_close_point() {
                nodes.push((coord.x, coord.y));
            }
        }
        nodes
    }

    /// Why a grid was refused. `Object` carries no `Debug`, so the objects
    /// are dropped before the result is unwrapped the other way up.
    fn refused(
        grid: &SymbolGrid,
        symbols: &[Entry],
        placement: &Placement,
        simplify: &Simplify,
    ) -> String {
        to_objects(grid, symbols, placement, simplify)
            .map(|objects| objects.len())
            .expect_err("the grid is not one which can be drawn")
    }

    fn objects_of(rows: &[&str], tolerance: f64) -> Vec<Object> {
        let grid = grid(rows);
        let placement = placement(grid.width, grid.height);
        let simplify = Simplify {
            tolerance,
            ..Simplify::default()
        };
        to_objects(&grid, &symbols(4), &placement, &simplify).expect("a grid of known classes")
    }

    #[test]
    fn a_single_cell_comes_out_as_four_rounded_corners() {
        let objects = objects_of(&["...", ".0.", "..."], 0.0);
        assert_eq!(objects.len(), 1);
        // Four sides of one cell each, so a node at the middle of each and
        // every one of them a corner.
        assert_eq!(
            nodes(&objects[0]),
            [(1.5, 1.0), (2.0, 1.5), (1.5, 2.0), (1.0, 1.5)]
        );
        // Every segment is a curve: three coordinates each, and the last of
        // them repeats the first node to close the part.
        assert_eq!(objects[0].coords.len(), 4 * 3 + 1);
        let last = objects[0].coords.last().expect("a closed part");
        assert!(last.is_close_point(), "{last:?}");
    }

    /// A straight run keeps two nodes however long it is, half a cell in from
    /// each end: that is the whole of the second step.
    #[test]
    fn a_straight_run_keeps_two_nodes_rather_than_one_per_cell() {
        let objects = objects_of(&[".....", ".000.", "....."], 0.0);
        assert_eq!(
            nodes(&objects[0]),
            [
                (1.5, 1.0),
                (3.5, 1.0),
                (4.0, 1.5),
                (3.5, 2.0),
                (1.5, 2.0),
                (1.0, 1.5),
            ]
        );
    }

    /// The drawing this module was written from: the ten by ten grid of
    /// `embedding_to_xml.svg`, whose nodes are what `step_3.svg` holds.
    #[test]
    fn the_reference_drawing_comes_out_as_it_was_drawn() {
        // The blue region of the drawing, rasterized: everything inside the
        // boundary its path traces. The rest of the square is the red one.
        let objects = objects_of(
            &[
                "1111111111",
                "1111101111",
                "1110000011",
                "1100000011",
                "1100010011",
                "1000110000",
                "1000110000",
                "1111110000",
                "1111110011",
                "1111111111",
            ],
            0.0,
        );
        assert_eq!(objects.len(), 2);
        let blue = objects
            .iter()
            .find(|object| object.symbol_id == 0)
            .expect("the region inside the boundary");
        // The nodes of `step_3.svg`, from wherever the tracing happens to
        // start rather than from wherever Inkscape did.
        assert_eq!(
            nodes(blue),
            [
                (5.5, 1.0),
                (6.0, 1.5),
                (6.5, 2.0),
                (7.5, 2.0),
                (8.0, 2.5),
                (8.0, 4.5),
                (8.5, 5.0),
                (9.5, 5.0),
                (10.0, 5.5),
                (10.0, 7.5),
                (9.5, 8.0),
                (8.5, 8.0),
                (8.0, 8.5),
                (7.5, 9.0),
                (6.5, 9.0),
                (6.0, 8.5),
                (6.0, 4.5),
                (5.5, 4.0),
                (5.0, 4.5),
                (4.5, 5.0),
                (4.0, 5.5),
                (4.0, 6.5),
                (3.5, 7.0),
                (1.5, 7.0),
                (1.0, 6.5),
                (1.0, 5.5),
                (1.5, 5.0),
                (2.0, 4.5),
                (2.0, 3.5),
                (2.5, 3.0),
                (3.0, 2.5),
                (3.5, 2.0),
                (4.5, 2.0),
                (5.0, 1.5),
            ]
        );

        // The red region is the rest of the square. The blue one reaches the
        // right edge of it, so red goes round rather than round and back:
        // one object of one part, not an object with a hole.
        let red = objects
            .iter()
            .find(|object| object.symbol_id == 1)
            .expect("the region around it");
        assert_eq!(
            red.coords
                .iter()
                .filter(|coord| coord.is_hole_point())
                .count(),
            0
        );
    }

    #[test]
    fn a_region_around_another_is_one_object_with_a_hole() {
        let objects = objects_of(&["000", "010", "000"], 0.0);
        assert_eq!(objects.len(), 2);
        let ring = objects
            .iter()
            .find(|object| object.symbol_id == 0)
            .expect("the surrounding region");
        // Two parts, and the coordinate before the second says so.
        let hole = ring
            .coords
            .iter()
            .filter(|coord| coord.is_hole_point())
            .count();
        assert_eq!(hole, 1, "{:?}", ring.coords);
        assert_eq!(
            ring.coords
                .iter()
                .filter(|coord| coord.flags & coord_flag::CLOSE_POINT != 0)
                .count(),
            2
        );
    }

    /// Two cells of one class touching at a corner alone are two regions:
    /// 4-connected is what a region is, and the shapes part company there.
    #[test]
    fn cells_which_touch_at_a_corner_alone_are_two_regions() {
        let objects = objects_of(&["0.", ".0"], 0.0);
        assert_eq!(objects.len(), 2);
    }

    /// A grid of nothing but single-cell regions costs the grid once, not once
    /// per region.
    ///
    /// The cost is what this is about, and the object count is only how it is
    /// checked: a chequerboard is the worst a grid can be — as many regions as
    /// it has cells to spare — and tracing each region's boundary by looking
    /// for its cells over the whole grid made that quadratic. This grid is a
    /// hundred thousand cells, which is a hair of work per region and some
    /// 10^9 the other way; a picture of a map is twenty-five times larger
    /// again, and that is a run which never comes out of its first epoch of
    /// image validation.
    #[test]
    fn a_grid_of_many_regions_is_vectorized_in_one_pass_over_it() {
        let side = 320;
        let mut grid = SymbolGrid::new(side, side);
        let mut regions = 0;
        for row in 0..side {
            for column in 0..side {
                if (row + column) % 2 == 0 {
                    grid.class[row * side + column] = 0;
                    regions += 1;
                } else {
                    grid.class[row * side + column] = BACKGROUND;
                }
            }
        }

        let objects = to_objects(
            &grid,
            &symbols(4),
            &placement(side, side),
            &Simplify::default(),
        )
        .expect("a grid of known classes");
        // Every cell of the chequerboard on its own, since touching at a
        // corner alone is not being joined.
        assert_eq!(objects.len(), regions);
    }

    /// A pinch inside one region is a pinch rather than a parting: the cells
    /// are joined the long way round, so the boundary runs through the corner
    /// and what the corner shut in becomes a hole.
    #[test]
    fn a_pinch_inside_one_region_leaves_a_hole() {
        let objects = objects_of(&["000", "0.0", "000"], 0.0);
        assert_eq!(objects.len(), 1);
        assert_eq!(
            objects[0]
                .coords
                .iter()
                .filter(|coord| coord.is_hole_point())
                .count(),
            1
        );
    }

    #[test]
    fn a_class_past_the_symbol_list_is_refused() {
        let grid = grid(&["9"]);
        let placement = placement(1, 1);
        let error = refused(&grid, &symbols(4), &placement, &Simplify::default());
        assert!(error.contains("symbol 9 of 4"), "{error}");
    }

    #[test]
    fn a_grid_whose_vectors_are_the_wrong_length_is_refused() {
        let mut grid = SymbolGrid::new(2, 2);
        grid.class.pop();
        let error = refused(&grid, &symbols(1), &placement(2, 2), &Simplify::default());
        assert!(error.contains("want 4 classes"), "{error}");

        let error = refused(
            &SymbolGrid::new(0, 4),
            &symbols(1),
            &placement(1, 1),
            &Simplify::default(),
        );
        assert!(error.contains("no grid at all"), "{error}");
    }

    /// The frame of a network's answer is the last class rather than the
    /// sentinel a file writes, and a grid says so before it is vectorized.
    #[test]
    fn the_frame_class_of_a_network_becomes_the_background() {
        let mut grid = grid(&["01", "11"]);
        grid.frame_is(1);
        let objects = to_objects(&grid, &symbols(2), &placement(2, 2), &Simplify::default())
            .expect("one class");
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].symbol_id, 0);
    }

    /// A region takes the angle of the cells it was made of, and only where
    /// the symbol has a pattern to turn.
    #[test]
    fn a_region_takes_the_angle_of_its_cells() {
        let mut grid = grid(&["00", "00"]);
        grid.rotation = vec![0.25; 4];
        let objects = to_objects(&grid, &symbols(2), &placement(2, 2), &Simplify::default())
            .expect("one class");
        assert!(
            (objects[0].rotation - TAU / 4.0).abs() < 1e-9,
            "{}",
            objects[0].rotation
        );
        let ObjectKind::Path(path) = &objects[0].kind else {
            panic!("an area is a path object");
        };
        assert!((path.pattern_rotation - TAU / 4.0).abs() < 1e-9);

        // Symbol 1 has no pattern to turn, so it is given no angle whatever
        // the cells say.
        let mut grid = grid_of_ones();
        grid.rotation = vec![0.25; 4];
        let objects = to_objects(&grid, &symbols(2), &placement(2, 2), &Simplify::default())
            .expect("one class");
        assert_eq!(objects[0].rotation, 0.0);
    }

    fn grid_of_ones() -> SymbolGrid {
        grid(&["11", "11"])
    }

    /// One symbol turned two ways is two objects: a region is a run of cells
    /// which agree about the angle as well as about the symbol.
    #[test]
    fn cells_of_one_symbol_turned_two_ways_are_two_regions() {
        let mut jumping = grid(&["00", "00"]);
        jumping.rotation = vec![0.1, 0.1, 0.6, 0.6];
        let objects = to_objects(
            &jumping,
            &symbols(1),
            &placement(2, 2),
            &Simplify::default(),
        )
        .expect("one class");
        assert_eq!(objects.len(), 2);
        let turns: Vec<f64> = objects.iter().map(|object| object.rotation / TAU).collect();
        assert!(
            turns.iter().any(|turn| (turn - 0.1).abs() < 1e-6)
                && turns.iter().any(|turn| (turn - 0.6).abs() < 1e-6),
            "{turns:?}"
        );

        // A field which drifts rather than jumps stays one region: the
        // comparison is between neighbours, not across the whole of it.
        let mut drifting = grid(&["0000"]);
        drifting.rotation = vec![0.10, 0.105, 0.11, 0.115];
        let objects = to_objects(
            &drifting,
            &symbols(1),
            &placement(4, 1),
            &Simplify::default(),
        )
        .expect("one class");
        assert_eq!(objects.len(), 1);
    }

    /// Never splitting on the angle is what a grid read off a network wants:
    /// a continuous field jitters, and a tight threshold would shatter every
    /// turning area into fragments.
    #[test]
    fn an_infinite_angle_threshold_never_splits_a_region() {
        let mut jumping = grid(&["00", "00"]);
        jumping.rotation = vec![0.1, 0.4, 0.7, 0.95];
        let simplify = Simplify {
            same_angle: f32::INFINITY,
            ..Simplify::default()
        };
        let objects =
            to_objects(&jumping, &symbols(1), &placement(2, 2), &simplify).expect("one class");
        assert_eq!(objects.len(), 1);
    }

    #[test]
    fn a_negative_angle_threshold_is_refused() {
        let error = refused(
            &grid(&["0"]),
            &symbols(1),
            &placement(1, 1),
            &Simplify {
                same_angle: -1.0,
                ..Simplify::default()
            },
        );
        assert!(error.contains("less than nothing"), "{error}");
    }

    /// The angle is averaged as a point on the circle, so a region whose
    /// cells sit either side of a whole turn comes out between them rather
    /// than halfway round from both.
    #[test]
    fn angles_are_averaged_across_the_seam_of_a_turn() {
        let mut grid = grid(&["00"]);
        grid.rotation = vec![0.99, 0.01];
        let objects = to_objects(&grid, &symbols(1), &placement(2, 1), &Simplify::default())
            .expect("one class");
        let turn = objects[0].rotation / TAU;
        assert!(!(0.01..=0.99).contains(&turn), "{turn}");
    }

    /// A tolerance takes nodes off a staircase, and nought takes none.
    #[test]
    fn a_tolerance_simplifies_a_staircase() {
        let staircase = [
            "1111111111",
            "0111111111",
            "0011111111",
            "0001111111",
            "0000111111",
            "0000011111",
            "0000001111",
            "0000000111",
            "0000000011",
            "0000000001",
        ];
        let exact = objects_of(&staircase, 0.0);
        let loose = objects_of(&staircase, 2.0);
        let count = |objects: &[Object]| objects.iter().map(|o| o.coords.len()).sum::<usize>();
        assert!(
            count(&loose) < count(&exact) / 2,
            "{} against {}",
            count(&loose),
            count(&exact)
        );
        // And it is still the same two regions, with the same symbols.
        assert_eq!(loose.len(), exact.len());
    }

    #[test]
    fn a_negative_tolerance_is_refused() {
        let error = refused(
            &grid(&["0"]),
            &symbols(1),
            &placement(1, 1),
            &Simplify {
                tolerance: -1.0,
                ..Simplify::default()
            },
        );
        assert!(error.contains("less than nothing"), "{error}");
    }

    /// A grid vectorized and rasterized again is the grid it was: the corner
    /// rounding moves the boundary by half a cell, and nothing else does.
    #[test]
    fn what_is_vectorized_rasterizes_back_to_what_it_was() {
        use crate::ground_truth::{Ground, GroundTruth};
        use tiny_skia::Transform;

        let rows = [
            "0000000011",
            "0000001111",
            "0000011111",
            "0002211111",
            "0022221111",
            "0222222111",
            "2222222211",
            "2222222221",
            "2222222222",
            "2222222222",
        ];
        let grid = grid(&rows);
        // One cell of the grid is one mm on the paper at 1:1000, and the
        // labels are drawn at ten pixels to the cell so that half a cell of
        // rounding is something to count.
        let objects = objects_of(&rows, 0.0);
        let grounds: Vec<Ground> = objects
            .iter()
            .map(|object| Ground {
                class: object.symbol_id as usize,
                turn: None,
                outline: object.coords.clone(),
            })
            .collect();
        let truth =
            GroundTruth::rasterize(&grounds, 4, 100, 100, Transform::from_scale(10.0, 10.0))
                .expect("the shapes are inside the image");

        let mut agreed = 0;
        for row in 0..100 {
            for column in 0..100 {
                let was = grid.class_at(column / 10, row / 10);
                if truth.class_of[row * 100 + column] == was {
                    agreed += 1;
                }
            }
        }
        assert!(agreed >= 9900, "{agreed} of 10000 pixels agreed");
    }
}
