//! Turning what a network says about the point symbols of a picture back
//! into point objects of a map — the way back from [`crate::point_truth`].
//!
//! The network answers, for every point symbol and every pixel, how sure it
//! is that a point of that symbol sits there: a [`PointGrid`], one heatmap
//! per symbol, which it was trained to make peak at one where a point is and
//! fall away around it. A point is brought back from it in three steps:
//!
//! 1. **Peaks**: a pixel is a point where it is at least
//!    the threshold and no lower than any of its eight neighbours — the
//!    non-maximum suppression of a keypoint detector, which is what keeps
//!    the ring around a peak from being taken for points of its own. Of two
//!    neighbours exactly as high, the first in reading order is kept.
//! 2. **Refined** to a fraction of a pixel: a parabola through the peak and
//!    its two neighbours, across and down, has its top where the point most
//!    likely is.
//! 3. **Placed** on the ground the picture covers, as the areas and the
//!    lines are, and written as a point object of the channel's symbol.
//!
//! A point object is given no angle: the symbols the network has learned so
//! far are the ones which do not turn.

use crate::map::{Object, ObjectKind, Point};
use crate::path_builder::coord_at;
use crate::symbol_kinds::Entry;
use crate::vectorize::Placement;

/// How sure the network has to be of a point, by default: the height its
/// heatmap has to reach at the peak.
///
/// Under a half, as keypoint detectors have it: a heatmap is trained against
/// Gaussians whose peak is one and whose shoulders are not, so a network
/// unsure of exactly which pixel the point is in spreads its peak and lowers
/// it, and the point is still there.
pub const DEFAULT_POINT_THRESHOLD: f32 = 0.3;

/// A heatmap per point symbol: how sure the network is that a point of that
/// symbol sits at each pixel.
#[derive(Clone, Debug)]
pub struct PointGrid {
    /// How many pixels across the picture is.
    pub width: usize,
    /// How many pixels down.
    pub height: usize,
    /// How many point symbols there are heatmaps for.
    pub classes: usize,
    /// The heatmaps, symbol after symbol and each of them row by row, from
    /// nought to one.
    pub heat: Vec<f32>,
}

impl PointGrid {
    /// A grid with no point anywhere.
    pub fn new(width: usize, height: usize, classes: usize) -> PointGrid {
        PointGrid {
            width,
            height,
            classes,
            heat: vec![0.0; width * height * classes],
        }
    }

    /// The heatmap of point symbol `class`.
    pub fn channel(&self, class: usize) -> &[f32] {
        let pixels = self.width * self.height;
        &self.heat[class * pixels..(class + 1) * pixels]
    }

    /// The heatmap of point symbol `class`, to write into.
    pub fn channel_mut(&mut self, class: usize) -> &mut [f32] {
        let pixels = self.width * self.height;
        &mut self.heat[class * pixels..(class + 1) * pixels]
    }
}

/// One point found in a heatmap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    /// The point symbol: a place in
    /// [`crate::symbol_kinds::Catalogue::points`].
    pub class: usize,
    /// Where it is, in pixels rightwards from the picture's left edge: the
    /// middle of pixel `i` is `i + 0.5`.
    pub x: f64,
    /// The same, downwards from the top edge.
    pub y: f64,
    /// How high the heatmap was at the peak.
    pub score: f32,
}

/// Every point of a grid: the peaks of each heatmap at least `threshold`
/// high, symbol by symbol and each in reading order — see the module
/// documentation.
pub fn peaks(grid: &PointGrid, threshold: f32) -> Vec<Peak> {
    let (width, height) = (grid.width, grid.height);
    let mut found = Vec::new();
    for class in 0..grid.classes {
        let heat = grid.channel(class);
        let at = |x: isize, y: isize| -> f32 {
            if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
                f32::NEG_INFINITY
            } else {
                heat[y as usize * width + x as usize]
            }
        };
        for row in 0..height as isize {
            for column in 0..width as isize {
                let value = at(column, row);
                if value < threshold {
                    continue;
                }
                // No lower than any neighbour, and higher than those which
                // come before it in reading order: a plateau is one peak.
                let mut highest = true;
                for (dx, dy) in [
                    (-1, -1),
                    (0, -1),
                    (1, -1),
                    (-1, 0),
                    (1, 0),
                    (-1, 1),
                    (0, 1),
                    (1, 1),
                ] {
                    let other = at(column + dx, row + dy);
                    let before = dy < 0 || (dy == 0 && dx < 0);
                    if other > value || (before && other == value) {
                        highest = false;
                        break;
                    }
                }
                if !highest {
                    continue;
                }
                let x =
                    column as f64 + 0.5 + vertex(at(column - 1, row), value, at(column + 1, row));
                let y = row as f64 + 0.5 + vertex(at(column, row - 1), value, at(column, row + 1));
                found.push(Peak {
                    class,
                    x,
                    y,
                    score: value,
                });
            }
        }
    }
    found
}

/// Where the top of the parabola through three heights a pixel apart is,
/// from the middle one, held to half a pixel either way. Nought where there
/// is no parabola to fit: an edge of the picture, or three in a line.
fn vertex(before: f32, middle: f32, after: f32) -> f64 {
    if !(before.is_finite() && after.is_finite()) {
        return 0.0;
    }
    let curve = (before - 2.0 * middle + after) as f64;
    if curve >= -1e-9 {
        return 0.0;
    }
    (0.5 * (before - after) as f64 / curve).clamp(-0.5, 0.5)
}

/// The point objects `found` comes to, each drawn with the symbol of its
/// channel, on a picture of `width` by `height` pixels laid over the ground
/// as `placement` says.
///
/// `symbols` is [`crate::symbol_kinds::Catalogue::points`] of the symbol set
/// the network was trained against, in the order `classes.json` recorded.
pub fn objects_of_points(
    found: &[Peak],
    symbols: &[Entry],
    width: usize,
    height: usize,
    placement: &Placement,
) -> Result<Vec<Object>, String> {
    let mm_per_meter = 1000.0 / placement.scale_denominator as f64;
    let cell = (
        placement.ground.width() / width as f64,
        placement.ground.height() / height as f64,
    );
    found
        .iter()
        .map(|peak| {
            let entry = symbols.get(peak.class).ok_or_else(|| {
                format!(
                    "a point of symbol {} was found and the symbol set holds {} point symbols",
                    peak.class,
                    symbols.len()
                )
            })?;
            let on_ground = Point::new(
                placement.ground.left() + peak.x * cell.0,
                placement.ground.top() + peak.y * cell.1,
            );
            let mut object = Object::new(ObjectKind::Point);
            object.coords = vec![coord_at(on_ground * mm_per_meter)];
            object.symbol_index = Some(entry.index);
            object.symbol_id = entry.id;
            Ok(object)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point_truth::{PointMark, PointTruth};

    /// A grid of one symbol out of the heatmaps of [`PointTruth`], which is
    /// what a network that learned them perfectly would say.
    fn grid_of(points: &[(f64, f64)], size: usize) -> PointGrid {
        let truth = PointTruth {
            height: size as u32,
            width: size as u32,
            classes: 1,
            points: points
                .iter()
                .map(|&(x, y)| PointMark { class: 0, x, y })
                .collect(),
        };
        PointGrid {
            width: size,
            height: size,
            classes: 1,
            heat: truth.heatmap(0, 0, size),
        }
    }

    #[test]
    fn a_perfect_heatmap_gives_back_its_points() {
        let grid = grid_of(&[(5.5, 6.5), (20.2, 11.8)], 32);
        let found = peaks(&grid, DEFAULT_POINT_THRESHOLD);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!((found[0].x - 5.5).abs() < 0.3 && (found[0].y - 6.5).abs() < 0.3);
        assert!((found[1].x - 20.2).abs() < 0.6 && (found[1].y - 11.8).abs() < 0.6);
    }

    #[test]
    fn nothing_over_the_threshold_is_no_point() {
        let mut grid = grid_of(&[(5.5, 6.5)], 16);
        for value in &mut grid.heat {
            *value *= 0.2;
        }
        assert!(peaks(&grid, 0.3).is_empty());
    }

    #[test]
    fn a_plateau_is_one_point() {
        let mut grid = PointGrid::new(8, 8, 1);
        grid.heat[3 * 8 + 3] = 0.8;
        grid.heat[3 * 8 + 4] = 0.8;
        assert_eq!(peaks(&grid, 0.5).len(), 1);
    }

    #[test]
    fn a_point_lands_on_the_ground_the_picture_covers() {
        use crate::geometry::Rect;
        let entry = Entry {
            index: 4,
            id: 42,
            code: "417".to_string(),
            name: "Tree".to_string(),
            turns: false,
            symmetry: 1,
        };
        // A hundred pixels over a hundred meters, at 1:1000: a pixel a meter
        // and a meter a millimetre.
        let placement = Placement {
            ground: Rect::from_ltrb(-50.0, -50.0, 50.0, 50.0),
            scale_denominator: 1000,
        };
        let found = [Peak {
            class: 0,
            x: 60.0,
            y: 25.0,
            score: 1.0,
        }];
        let objects = objects_of_points(&found, &[entry], 100, 100, &placement).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].symbol_index, Some(4));
        assert!((objects[0].coords[0].x - 10.0).abs() < 1e-6);
        assert!((objects[0].coords[0].y + 25.0).abs() < 1e-6);
        assert!(objects_of_points(
            &[Peak {
                class: 3,
                ..found[0]
            }],
            &[],
            100,
            100,
            &placement
        )
        .is_err());
    }
}
