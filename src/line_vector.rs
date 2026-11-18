//! Turning what a network says about the lines of a picture back into line
//! objects of a map — the way back from [`crate::line_truth`].
//!
//! The network answers, for every line symbol and every pixel, whether a line
//! of that symbol runs through it: a [`LineGrid`], one yes-or-no mask per
//! symbol. A mask is a band a few pixels wide along where the line is, not
//! the line, so each is brought back to one in four steps:
//!
//! 1. **Thinned** to a one-pixel skeleton ([Zhang–Suen]), which is the middle
//!    of the band wherever it is wide and leaves it alone where it is not.
//! 2. **Traced**: the skeleton's pixels form a graph, with a node wherever a
//!    line ends or meets another, and every run of pixels between two nodes
//!    is one path. A loop with no node on it is a path which comes back to
//!    where it began.
//! 3. **Filtered**: a path shorter than `min_length` pixels is a speck and a
//!    spur, not a line, and is dropped.
//! 4. **Simplified** by Douglas–Peucker to within `tolerance` pixels, which
//!    is what takes the staircase of a diagonal run of pixels back out.
//!
//! Each path that is left is one object, drawn with the symbol of its mask,
//! open at both ends (or closed where it was a loop). Where two lines of one
//! symbol meet, they are two objects which share an end rather than one
//! object with a branch: a line object has no branches.
//!
//! The masks of different symbols are independent, so a pixel two lines cross
//! is on in both and both are traced.
//!
//! [Zhang–Suen]: https://en.wikipedia.org/wiki/Zhang-Suen_thinning_algorithm

use crate::map::{Object, ObjectKind, PathObject, Point};
use crate::path_builder::PathBuilder;
use crate::symbol_kinds::Entry;
use crate::vectorize::Placement;

/// What to keep of the lines a network found.
#[derive(Clone, Copy, Debug)]
pub struct LineSettings {
    /// How far a line may be moved to be rid of a node, in pixels.
    pub tolerance: f64,
    /// How long a path has to be to be a line, in pixels. Shorter ones are
    /// specks and the spurs a thinning leaves at the end of a band.
    pub min_length: f64,
}

impl Default for LineSettings {
    fn default() -> LineSettings {
        LineSettings {
            tolerance: 1.0,
            min_length: 6.0,
        }
    }
}

/// A yes-or-no per pixel for each line symbol: where the network says a line
/// of that symbol runs.
#[derive(Clone, Debug)]
pub struct LineGrid {
    /// How many pixels across the picture is.
    pub width: usize,
    /// How many pixels down.
    pub height: usize,
    /// How many line symbols there are masks for.
    pub classes: usize,
    /// The masks, symbol after symbol and each of them row by row: `1` where
    /// a line of that symbol runs through the pixel and `0` where not.
    pub mask: Vec<u8>,
}

impl LineGrid {
    /// A grid with no line anywhere.
    pub fn new(width: usize, height: usize, classes: usize) -> LineGrid {
        LineGrid {
            width,
            height,
            classes,
            mask: vec![0; width * height * classes],
        }
    }

    /// The mask of line symbol `class`.
    pub fn channel(&self, class: usize) -> &[u8] {
        let pixels = self.width * self.height;
        &self.mask[class * pixels..(class + 1) * pixels]
    }

    /// The mask of line symbol `class`, to be written.
    pub fn channel_mut(&mut self, class: usize) -> &mut [u8] {
        let pixels = self.width * self.height;
        &mut self.mask[class * pixels..(class + 1) * pixels]
    }

    /// How many line symbols have any pixel on.
    pub fn symbols_used(&self) -> usize {
        (0..self.classes)
            .filter(|&class| self.channel(class).contains(&1))
            .count()
    }
}

/// The line objects a [`LineGrid`] comes to: one per path of each symbol's
/// skeleton, in the symbol's order.
///
/// `symbols` is [`crate::symbol_kinds::Catalogue::lines`] of the symbol set
/// the network was trained against, in the order `classes.json` recorded;
/// `placement` is the one the areas are written with, so that a line lands
/// on the ground it was read off.
pub fn objects_of_lines(
    grid: &LineGrid,
    symbols: &[Entry],
    placement: &Placement,
    settings: &LineSettings,
) -> Result<Vec<Object>, String> {
    if grid.classes != symbols.len() {
        return Err(format!(
            "the network answers for {} line symbols and the symbol set holds {}",
            grid.classes,
            symbols.len()
        ));
    }
    if settings.tolerance.is_nan() || settings.tolerance < 0.0 {
        return Err(format!(
            "the tolerance cannot be less than nothing, and is {} pixels",
            settings.tolerance
        ));
    }

    let mm_per_meter = 1000.0 / placement.scale_denominator as f64;
    let cell = (
        placement.ground.width() / grid.width as f64,
        placement.ground.height() / grid.height as f64,
    );
    let at = |(x, y): (f64, f64)| {
        Point::new(
            placement.ground.left() + x * cell.0,
            placement.ground.top() + y * cell.1,
        )
    };

    let mut objects = Vec::new();
    for (class, entry) in symbols.iter().enumerate() {
        let mask = grid.channel(class);
        if !mask.contains(&1) {
            continue;
        }
        for path in polylines(mask, grid.width, grid.height, settings) {
            let mut builder = PathBuilder::new(mm_per_meter);
            builder.move_to(at(path.points[0]));
            for &point in &path.points[1..] {
                builder.line_to(at(point));
            }
            if path.closed {
                builder.close();
            }
            let mut object = Object::new(ObjectKind::Path(PathObject::default()));
            object.coords = builder.coords;
            object.symbol_index = Some(entry.index);
            object.symbol_id = entry.id;
            objects.push(object);
        }
    }
    Ok(objects)
}

/// One line found in a mask, in pixels: the middle of each pixel it runs
/// through is `(column + 0.5, row + 0.5)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Polyline {
    /// The corners of the line, at least two.
    pub points: Vec<(f64, f64)>,
    /// Whether it comes back to where it began.
    pub closed: bool,
}

/// The lines of one mask: thinned, traced, filtered and simplified — see the
/// module documentation.
pub fn polylines(
    mask: &[u8],
    width: usize,
    height: usize,
    settings: &LineSettings,
) -> Vec<Polyline> {
    assert_eq!(mask.len(), width * height, "a mask is one byte a pixel");
    let skeleton = thin(mask, width, height);
    let mut found = Vec::new();
    for path in trace(&skeleton, width, height) {
        if length_of(&path.pixels) < settings.min_length {
            continue;
        }
        let centres: Vec<(f64, f64)> = path
            .pixels
            .iter()
            .map(|&(x, y)| (x as f64 + 0.5, y as f64 + 0.5))
            .collect();
        let points = if path.closed {
            peucker_loop(&centres, settings.tolerance)
        } else {
            peucker_open(&centres, settings.tolerance)
        };
        // A loop simplified down to two points is a line there and back.
        if points.len() < 2 || (path.closed && points.len() < 3) {
            continue;
        }
        found.push(Polyline {
            points,
            closed: path.closed,
        });
    }
    found
}

/// How long a run of pixels is, counting a diagonal step as it is.
fn length_of(pixels: &[(usize, usize)]) -> f64 {
    pixels
        .windows(2)
        .map(|pair| {
            let (dx, dy) = (
                pair[0].0 as f64 - pair[1].0 as f64,
                pair[0].1 as f64 - pair[1].1 as f64,
            );
            dx.hypot(dy)
        })
        .sum()
}

/// The eight neighbours of a pixel, clockwise from the one above:
/// `(dx, dy)`. Zhang–Suen's `P2..P9` are these in this order.
const AROUND: [(isize, isize); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// Zhang–Suen thinning of a mask to a skeleton one pixel wide. The picture's
/// edge counts as off, so a band which runs off the picture ends at its
/// edge.
fn thin(mask: &[u8], width: usize, height: usize) -> Vec<u8> {
    // Padded by a pixel of nothing all round, so that no pixel is on an edge
    // and every one has its eight neighbours to look at.
    let (padded_width, padded_height) = (width + 2, height + 2);
    let mut image = vec![0u8; padded_width * padded_height];
    for row in 0..height {
        for column in 0..width {
            image[(row + 1) * padded_width + column + 1] = (mask[row * width + column] != 0) as u8;
        }
    }

    let neighbours = |image: &[u8], at: usize| -> [u8; 8] {
        let mut around = [0u8; 8];
        for (n, &(dx, dy)) in AROUND.iter().enumerate() {
            let index = (at as isize + dy * padded_width as isize + dx) as usize;
            around[n] = image[index];
        }
        around
    };

    loop {
        let mut changed = false;
        for step in 0..2 {
            let mut remove = Vec::new();
            for row in 1..=height {
                for column in 1..=width {
                    let at = row * padded_width + column;
                    if image[at] == 0 {
                        continue;
                    }
                    let p = neighbours(&image, at);
                    let count: u8 = p.iter().sum();
                    if !(2..=6).contains(&count) {
                        continue;
                    }
                    // How many times the ring goes from off to on.
                    let transitions = (0..8).filter(|&n| p[n] == 0 && p[(n + 1) % 8] == 1).count();
                    if transitions != 1 {
                        continue;
                    }
                    // P2 = up, P4 = right, P6 = down, P8 = left.
                    let (up, right, down, left) = (p[0], p[2], p[4], p[6]);
                    let kept = if step == 0 {
                        up * right * down == 0 && right * down * left == 0
                    } else {
                        up * right * left == 0 && up * down * left == 0
                    };
                    if kept {
                        remove.push(at);
                    }
                }
            }
            changed |= !remove.is_empty();
            for at in remove {
                image[at] = 0;
            }
        }
        if !changed {
            break;
        }
    }

    let mut skeleton = vec![0u8; width * height];
    for row in 0..height {
        for column in 0..width {
            skeleton[row * width + column] = image[(row + 1) * padded_width + column + 1];
        }
    }
    skeleton
}

/// A run of skeleton pixels between two nodes, or round a loop.
struct Path {
    pixels: Vec<(usize, usize)>,
    closed: bool,
}

/// Follows a skeleton into its paths.
///
/// A pixel is told apart by how many separate *arms* leave it, which is how
/// many runs of skeleton pixels there are in its ring of eight neighbours —
/// not by how many neighbours it has. A staircase corner of a diagonal line
/// has three neighbours and two arms, since two of the three touch each
/// other; counting neighbours would take every one of those for a junction
/// and cut the line into pieces at each step.
///
/// One arm is the end of a line, two is the middle of one, three or more is
/// where lines meet.
fn trace(skeleton: &[u8], width: usize, height: usize) -> Vec<Path> {
    let on = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < width
            && (y as usize) < height
            && skeleton[y as usize * width + x as usize] != 0
    };
    let pixel = |at: usize| (at % width, at / width);

    // The arms leaving a skeleton pixel: each a run of consecutive neighbours
    // in the ring, as the flat indices of its pixels.
    let arms = |at: usize| -> Vec<Vec<(usize, usize)>> {
        let (x, y) = ((at % width) as isize, (at / width) as isize);
        let ring: Vec<bool> = AROUND.iter().map(|&(dx, dy)| on(x + dx, y + dy)).collect();
        let mut arms = Vec::new();
        for n in 0..8 {
            if ring[n] && !ring[(n + 7) % 8] {
                let mut run = Vec::new();
                let mut k = n;
                while ring[k % 8] && run.len() < 8 {
                    let (dx, dy) = AROUND[k % 8];
                    run.push((k % 8, ((y + dy) as usize) * width + (x + dx) as usize));
                    k += 1;
                }
                arms.push(run);
            }
        }
        arms
    };
    // Where to step to along an arm: the pixel straight up, down, left or
    // right of this one where the arm has such a one, which keeps a path from
    // cutting a corner it need not.
    let step_of = |arm: &[(usize, usize)]| -> usize {
        arm.iter()
            .find(|(direction, _)| direction % 2 == 0)
            .unwrap_or(&arm[0])
            .1
    };

    let arm_count: Vec<usize> = (0..skeleton.len())
        .map(|at| if skeleton[at] != 0 { arms(at).len() } else { 0 })
        .collect();
    // A node is where a line ends or lines meet.
    let is_node = |at: usize| skeleton[at] != 0 && arm_count[at] != 2 && arm_count[at] != 0;

    let mut visited = vec![false; skeleton.len()];
    let mut paths = Vec::new();

    // Walks on from `here`, having come from `before`, until a node or a
    // pixel already walked.
    let walk =
        |start: usize, first: usize, visited: &mut Vec<bool>| -> (Vec<(usize, usize)>, bool) {
            let mut pixels = vec![pixel(start), pixel(first)];
            let (mut before, mut here) = (start, first);
            loop {
                if is_node(here) {
                    return (pixels, false);
                }
                visited[here] = true;
                let ways = arms(here);
                // The arm which does not hold the pixel just come from.
                let onward = ways
                    .iter()
                    .find(|arm| !arm.iter().any(|&(_, p)| p == before))
                    .or_else(|| {
                        ways.iter()
                            .find(|arm| arm.iter().any(|&(_, p)| p != before))
                    });
                let Some(onward) = onward else {
                    return (pixels, false);
                };
                let next = step_of(onward);
                if next == before {
                    return (pixels, false);
                }
                if next == start {
                    pixels.push(pixel(next));
                    return (pixels, true);
                }
                if !is_node(next) && visited[next] {
                    return (pixels, false);
                }
                pixels.push(pixel(next));
                (before, here) = (here, next);
            }
        };

    for start in (0..skeleton.len()).filter(|&at| is_node(at)) {
        for arm in arms(start) {
            let first = step_of(&arm);
            // Between two nodes with nothing between them is the inside of a
            // junction, not a line, and a pixel already walked belongs to a
            // path found from its other end.
            if is_node(first) || visited[first] {
                continue;
            }
            let (pixels, closed) = walk(start, first, &mut visited);
            paths.push(Path { pixels, closed });
        }
    }

    // What is left has no node on it: loops, each a ring of pixels of two
    // arms.
    for start in 0..skeleton.len() {
        if skeleton[start] == 0 || visited[start] || is_node(start) || arm_count[start] != 2 {
            continue;
        }
        visited[start] = true;
        let ways = arms(start);
        let first = step_of(&ways[0]);
        if visited[first] {
            continue;
        }
        let (pixels, closed) = walk(start, first, &mut visited);
        paths.push(Path { pixels, closed });
    }
    paths
}

/// Douglas–Peucker over an open run of points: the ends are kept, and so is
/// every point which is more than `tolerance` from the line through its
/// neighbours' kept ends.
fn peucker_open(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    peucker(points, 0, points.len() - 1, tolerance, &mut keep);
    points
        .iter()
        .zip(keep)
        .filter_map(|(&p, kept)| kept.then_some(p))
        .collect()
}

/// Douglas–Peucker over a loop: split at the point furthest from the first,
/// and simplify the two halves as open runs, which keeps the first point as
/// a corner rather than wherever the trace happened to start.
fn peucker_loop(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() < 4 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let far = (1..points.len())
        .max_by(|&a, &b| distance(points[0], points[a]).total_cmp(&distance(points[0], points[b])))
        .expect("a loop has points");
    let mut first = peucker_open(&points[..=far], tolerance);
    let mut closing: Vec<(f64, f64)> = points[far..].to_vec();
    closing.push(points[0]);
    let second = peucker_open(&closing, tolerance);
    // `first` ends at the far point which `second` starts at, and `second`
    // ends at the first point again, which closing the loop supplies.
    first.extend_from_slice(&second[1..second.len() - 1]);
    first
}

fn peucker(points: &[(f64, f64)], from: usize, to: usize, tolerance: f64, keep: &mut [bool]) {
    if to <= from + 1 {
        return;
    }
    let (far, worst) = (from + 1..to)
        .map(|i| (i, off_line(points[i], points[from], points[to])))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("a run with a point between its ends");
    if worst > tolerance {
        keep[far] = true;
        peucker(points, from, far, tolerance, keep);
        peucker(points, far, to, tolerance, keep);
    }
}

fn off_line(point: (f64, f64), from: (f64, f64), to: (f64, f64)) -> f64 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    if length == 0.0 {
        return distance(point, from);
    }
    ((point.0 - from.0) * dy - (point.1 - from.1) * dx).abs() / length
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mask of `width` by `height` with the pixels `on` set.
    fn mask(width: usize, height: usize, on: impl Fn(usize, usize) -> bool) -> Vec<u8> {
        (0..width * height)
            .map(|at| on(at % width, at / width) as u8)
            .collect()
    }

    fn settings() -> LineSettings {
        LineSettings::default()
    }

    #[test]
    fn a_band_is_one_line_down_its_middle() {
        // A band five pixels thick along the middle of a 40 by 21 picture.
        let band = mask(40, 21, |x, y| (3..37).contains(&x) && (8..13).contains(&y));
        let lines = polylines(&band, 40, 21, &settings());
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(!line.closed);
        // Two points, the middle of the band all the way: y of about 10.5.
        assert_eq!(line.points.len(), 2, "{:?}", line.points);
        for &(_, y) in &line.points {
            assert!((y - 10.5).abs() < 1.5, "y = {y}");
        }
        assert!(line.points[0].0 < 8.0 && line.points[1].0 > 32.0);
    }

    #[test]
    fn a_speck_is_not_a_line() {
        let speck = mask(30, 30, |x, y| {
            (10..13).contains(&x) && (10..13).contains(&y)
        });
        assert!(polylines(&speck, 30, 30, &settings()).is_empty());
    }

    #[test]
    fn a_ring_is_one_closed_line() {
        let (cx, cy, radius) = (20.0f64, 20.0f64, 12.0f64);
        let ring = mask(40, 40, |x, y| {
            let d = (x as f64 - cx).hypot(y as f64 - cy);
            (radius - 1.5..radius + 1.5).contains(&d)
        });
        let lines = polylines(&ring, 40, 40, &settings());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].closed);
        assert!(lines[0].points.len() >= 6);
    }

    /// Two lines which cross are paths between the end of each and the
    /// crossing: four of them, which is what a line object (which cannot
    /// branch) can say about a cross.
    #[test]
    fn a_cross_is_four_lines_meeting_at_the_crossing() {
        let cross = mask(41, 41, |x, y| {
            ((19..22).contains(&y) && (2..39).contains(&x))
                || ((19..22).contains(&x) && (2..39).contains(&y))
        });
        let lines = polylines(&cross, 41, 41, &settings());
        assert!(
            (4..=8).contains(&lines.len()),
            "{} lines: {lines:?}",
            lines.len()
        );
        // Every one of them reaches the middle, or ends at one of the four
        // ends of the cross.
        let total: f64 = lines.iter().map(|l| length_of_points(&l.points)).sum();
        // Two bars of 37 pixels, a little shorter for the ends the thinning
        // eats.
        assert!(
            (60.0..80.0).contains(&total),
            "the cross is some 74 pixels of line: {total}"
        );
    }

    fn length_of_points(points: &[(f64, f64)]) -> f64 {
        points.windows(2).map(|w| distance(w[0], w[1])).sum()
    }

    #[test]
    fn a_diagonal_band_has_its_staircase_taken_out() {
        let band = mask(60, 60, |x, y| {
            (x as isize - y as isize).abs() <= 2 && (5..55).contains(&x)
        });
        let lines = polylines(&band, 60, 60, &settings());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].points.len() <= 4, "{:?}", lines[0].points);
    }

    #[test]
    fn the_symbol_with_no_pixel_on_makes_no_object() {
        use crate::geometry::Rect;
        let entry = |index| Entry {
            index,
            id: index as i32,
            code: "1".into(),
            name: "line".into(),
            turns: false,
            symmetry: 1,
        };
        let mut grid = LineGrid::new(40, 21, 2);
        for (at, on) in mask(40, 21, |x, y| (3..37).contains(&x) && (8..13).contains(&y))
            .into_iter()
            .enumerate()
        {
            grid.channel_mut(1)[at] = on;
        }
        let placement = Placement {
            ground: Rect::from_ltrb(-20.0, -10.0, 20.0, 10.0),
            scale_denominator: 10000,
        };
        let objects =
            objects_of_lines(&grid, &[entry(4), entry(7)], &placement, &settings()).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(
            objects[0].symbol_index,
            Some(7),
            "drawn with the second symbol"
        );
        assert_eq!(grid.symbols_used(), 1);
        // A grid for another number of symbols is refused rather than drawn
        // with the wrong ones.
        assert!(objects_of_lines(&grid, &[entry(4)], &placement, &settings()).is_err());
    }
}
