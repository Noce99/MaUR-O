//! The Contour Raster (`Contours-to-Raster.md`, Step 0): a 2D grid recording,
//! per pixel, which contour (if any) passes through it, plus Appendix 5's
//! supercover pixel-walk used to find contour intersections in Steps 0 and 2.

use geo::{Coord, LineString, Polygon};
use line_drawing::Supercover;

/// A grid recording, per pixel, the index (plus one; zero means "no
/// contour") of the contour that pixel was written by.
pub struct ContourRaster {
    grid: Vec<Vec<u32>>,
    /// World-space (ground meters) position of pixel `(0, 0)`'s low corner.
    pub origin: Coord<f64>,
    /// Pixel size, in ground meters.
    pub px_size: f64,
    /// Grid width, in pixels.
    pub width: usize,
    /// Grid height, in pixels.
    pub height: usize,
}

impl ContourRaster {
    /// An all-zero raster of `width` x `height` pixels, `px_size` meters
    /// each, with `origin` as the ground-meter position of pixel `(0, 0)`.
    pub fn new(origin: Coord<f64>, px_size: f64, width: usize, height: usize) -> ContourRaster {
        ContourRaster {
            grid: vec![vec![0u32; width]; height],
            origin,
            px_size,
            width,
            height,
        }
    }

    /// Converts a world-space Coord to raster pixel indices, using this
    /// raster's own origin and pixel size. See Appendix 5.
    pub fn to_px(&self, coord: Coord<f64>) -> (i64, i64) {
        (
            ((coord.x - self.origin.x) / self.px_size).floor() as i64,
            ((coord.y - self.origin.y) / self.px_size).floor() as i64,
        )
    }

    /// The world-space (ground meters) position of pixel `(x, y)`'s center.
    pub fn pixel_center(&self, x: i64, y: i64) -> Coord<f64> {
        Coord {
            x: self.origin.x + (x as f64 + 0.5) * self.px_size,
            y: self.origin.y + (y as f64 + 0.5) * self.px_size,
        }
    }

    /// The raw pixel value (0 = no contour, otherwise contour index + 1), or
    /// 0 for a pixel outside the raster.
    pub fn get(&self, x: i64, y: i64) -> u32 {
        if x < 0 || y < 0 {
            return 0;
        }
        let (x, y) = (x as usize, y as usize);
        self.grid
            .get(y)
            .and_then(|row| row.get(x))
            .copied()
            .unwrap_or(0)
    }

    fn set(&mut self, x: i64, y: i64, value: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if let Some(cell) = self.grid.get_mut(y).and_then(|row| row.get_mut(x)) {
            *cell = value;
        }
    }

    /// Writes `contour_idx` (as `contour_idx + 1`) to every pixel the
    /// densified `LineString` touches, walking each consecutive pair of
    /// points with a supercover traversal so a diagonally-crossed shared
    /// corner pixel is never skipped (Step 0). `Err`s, naming
    /// `rasterization_px_size` as the doc suggests, if a touched pixel
    /// already holds a *different* contour's index -- writing the same
    /// value again is a no-op, per the doc.
    pub fn write_contour(
        &mut self,
        contour_idx: u64,
        densified_ls: &LineString<f64>,
    ) -> Result<(), String> {
        let value = contour_idx as u32 + 1;
        let pts = &densified_ls.0;
        if pts.len() < 2 {
            return Ok(());
        }
        for w in pts.windows(2) {
            let a = self.to_px(w[0]);
            let b = self.to_px(w[1]);
            for (x, y) in Supercover::new(a, b) {
                let current = self.get(x, y);
                if current == 0 {
                    self.set(x, y, value);
                } else if current != value {
                    return Err(format!(
                        "the Contour Raster pixel at ({x}, {y}) is claimed by two different \
                         contours (index {} and index {}); decrease rasterization_px_size",
                        current - 1,
                        contour_idx
                    ));
                }
            }
        }
        Ok(())
    }

    /// Scans, without writing anything, every pixel `densified_ls` would
    /// touch if written as `contour_idx`, and returns the world-space center
    /// and existing contour index of *every* pixel that already holds a
    /// different contour's index (not just the first). A non-mutating
    /// counterpart to [`Self::write_contour`]'s own conflict check, so a
    /// caller can decide -- before committing any write -- whether to unify
    /// two contours instead of crashing. Checking every conflicting pixel,
    /// not just one, is what tells a long run of genuinely parallel,
    /// closely-spaced contours (which must still crash: decreasing
    /// `rasterization_px_size` is the only fix) apart from two pieces of the
    /// same physical line separated by a small digitizing gap, where the
    /// conflict is confined to a small cluster right at that gap (see
    /// `step0_extract::extract`'s contour-merging pass, which is the only
    /// caller that needs the full list).
    pub fn find_conflicts(
        &self,
        contour_idx: u64,
        densified_ls: &LineString<f64>,
    ) -> Vec<(Coord<f64>, u64)> {
        let value = contour_idx as u32 + 1;
        let pts = &densified_ls.0;
        let mut conflicts = Vec::new();
        if pts.len() < 2 {
            return conflicts;
        }
        for w in pts.windows(2) {
            let a = self.to_px(w[0]);
            let b = self.to_px(w[1]);
            for (x, y) in Supercover::new(a, b) {
                let current = self.get(x, y);
                if current != 0 && current != value {
                    conflicts.push((self.pixel_center(x, y), (current - 1) as u64));
                }
            }
        }
        conflicts
    }

    /// Walks every pixel the segment `prev -> next` traverses and returns the
    /// first contour index encountered (excluding `exclude_contour_idx`, a
    /// rain drop's own source contour), or `None` if the step is clear. See
    /// Appendix 5.
    ///
    /// Appendix 5's own reference code converts `prev` and `next` to pixel
    /// indices *before* handing them to `Supercover`, which only ever sees
    /// which cell each endpoint floors into, not where in that cell it
    /// actually sits, nor anything about the pixels in between beyond the
    /// two endpoint cells' own indices. That loses exactly the information
    /// needed to notice a thin, diagonally-placed contour a several-pixel-
    /// long step (`rain_drop_step` is only asked to stay "a few pixels at
    /// most") clips for a fraction of its length without either endpoint
    /// landing inside it. This walks the continuous segment directly in
    /// pixel space instead (a standard grid-traversal/DDA walk, computing
    /// exactly where it crosses each pixel boundary), so every cell the
    /// line geometrically touches is found regardless of how long the step
    /// is or where exactly within their own pixels the endpoints fall.
    pub fn first_hit_along_step(
        &self,
        prev: Coord<f64>,
        next: Coord<f64>,
        exclude_contour_idx: u64,
    ) -> Option<u64> {
        let mut hit = None;
        self.walk_pixels(prev, next, |x, y| {
            let val = self.get(x, y);
            if val == 0 {
                return true; // keep walking
            }
            let contour_idx = (val - 1) as u64;
            if contour_idx == exclude_contour_idx {
                return true;
            }
            hit = Some(contour_idx);
            false // found one, stop
        });
        hit
    }

    /// Calls `visit` with every pixel the segment `prev -> next` touches, in
    /// the order visited, stopping early if `visit` returns `false`. A
    /// proper continuous grid traversal (see [`Self::first_hit_along_step`]
    /// for why `Supercover` on the endpoints' own pixel indices is not
    /// enough): tracks the exact parametric position (`t`, 0 at `prev`, 1 at
    /// `next`) at which the segment next crosses a vertical or horizontal
    /// pixel boundary, and steps into whichever cell that crossing leads to
    /// -- both, if it passes exactly through a shared corner, the way
    /// `Supercover` itself is conservative about corner touches.
    fn walk_pixels(
        &self,
        prev: Coord<f64>,
        next: Coord<f64>,
        mut visit: impl FnMut(i64, i64) -> bool,
    ) {
        let fx0 = (prev.x - self.origin.x) / self.px_size;
        let fy0 = (prev.y - self.origin.y) / self.px_size;
        let fx1 = (next.x - self.origin.x) / self.px_size;
        let fy1 = (next.y - self.origin.y) / self.px_size;
        let (dx, dy) = (fx1 - fx0, fy1 - fy0);

        let (mut x, mut y) = (fx0.floor() as i64, fy0.floor() as i64);
        let (end_x, end_y) = (fx1.floor() as i64, fy1.floor() as i64);

        if !visit(x, y) {
            return;
        }
        if x == end_x && y == end_y {
            return;
        }

        let step_x = if dx > 0.0 {
            1
        } else if dx < 0.0 {
            -1
        } else {
            0
        };
        let step_y = if dy > 0.0 {
            1
        } else if dy < 0.0 {
            -1
        } else {
            0
        };
        let t_delta_x = if dx != 0.0 {
            (1.0 / dx).abs()
        } else {
            f64::INFINITY
        };
        let t_delta_y = if dy != 0.0 {
            (1.0 / dy).abs()
        } else {
            f64::INFINITY
        };
        let mut t_max_x = if dx > 0.0 {
            ((x + 1) as f64 - fx0) / dx
        } else if dx < 0.0 {
            (x as f64 - fx0) / dx
        } else {
            f64::INFINITY
        };
        let mut t_max_y = if dy > 0.0 {
            ((y + 1) as f64 - fy0) / dy
        } else if dy < 0.0 {
            (y as f64 - fy0) / dy
        } else {
            f64::INFINITY
        };

        const CORNER_EPSILON: f64 = 1e-9;
        loop {
            if (t_max_x - t_max_y).abs() < CORNER_EPSILON {
                // Passes exactly through the shared corner of four cells:
                // touches both flanking cells, not just whichever axis a
                // tie-break would otherwise favor, before the diagonal one.
                if !visit(x + step_x, y) {
                    return;
                }
                if !visit(x, y + step_y) {
                    return;
                }
                x += step_x;
                y += step_y;
                t_max_x += t_delta_x;
                t_max_y += t_delta_y;
            } else if t_max_x < t_max_y {
                x += step_x;
                t_max_x += t_delta_x;
            } else {
                y += step_y;
                t_max_y += t_delta_y;
            }
            if !visit(x, y) {
                return;
            }
            if x == end_x && y == end_y {
                return;
            }
            if t_max_x > 1.0 && t_max_y > 1.0 {
                return; // reached (or passed) `next`; nothing further to visit
            }
        }
    }

    /// The index of the contour whose nearest pixel (by pixel-center
    /// distance) to `pos` lies within `radius` ground meters, or `None` if
    /// no contour has one that close. A Slope Line's own position is not
    /// always pixel-exact on top of its contour, so Step 0 uses this
    /// (`slope_lines_contours_search_radius`) instead of a single
    /// under-the-point pixel lookup. Scans the pixel-space bounding box of
    /// the search circle; a tie between two different contours' pixels at
    /// the same distance resolves to whichever is scanned first (row-major),
    /// an edge case `radius` is meant to stay small enough to make
    /// vanishingly unlikely to matter in practice.
    pub fn nearest_contour_within_radius(&self, pos: Coord<f64>, radius: f64) -> Option<u64> {
        let (px_min_x, px_min_y) = self.to_px(Coord {
            x: pos.x - radius,
            y: pos.y - radius,
        });
        let (px_max_x, px_max_y) = self.to_px(Coord {
            x: pos.x + radius,
            y: pos.y + radius,
        });
        let mut best: Option<(f64, u64)> = None;
        for y in px_min_y..=px_max_y {
            for x in px_min_x..=px_max_x {
                let val = self.get(x, y);
                if val == 0 {
                    continue;
                }
                let center = self.pixel_center(x, y);
                let dist = (center.x - pos.x).hypot(center.y - pos.y);
                if dist > radius {
                    continue;
                }
                if best.is_none_or(|(best_dist, _)| dist < best_dist) {
                    best = Some((dist, (val - 1) as u64));
                }
            }
        }
        best.map(|(_, idx)| idx)
    }

    /// Every pixel (by index) whose center falls inside `poly`, found by
    /// scanning the polygon's own pixel-space bounding box. The doc gives no
    /// code for rasterizing an *area* -- Appendix 5's supercover walk only
    /// answers which pixels a *line* touches -- so this is new: cheap here
    /// since the polygons Step 1 rasterizes are thin buffered Jump lines,
    /// not large area fills.
    pub fn pixels_in_polygon(&self, poly: &Polygon<f64>) -> Vec<(i64, i64)> {
        let exterior = poly.exterior();
        let Some(first) = exterior.0.first() else {
            return Vec::new();
        };
        let (mut min_x, mut min_y) = (first.x, first.y);
        let (mut max_x, mut max_y) = (first.x, first.y);
        for c in &exterior.0 {
            min_x = min_x.min(c.x);
            min_y = min_y.min(c.y);
            max_x = max_x.max(c.x);
            max_y = max_y.max(c.y);
        }
        let (px_min_x, px_min_y) = self.to_px(Coord { x: min_x, y: min_y });
        let (px_max_x, px_max_y) = self.to_px(Coord { x: max_x, y: max_y });

        let mut hits = Vec::new();
        for y in px_min_y..=px_max_y {
            for x in px_min_x..=px_max_x {
                if geo::algorithm::Contains::contains(poly, &self.pixel_center(x, y)) {
                    hits.push((x, y));
                }
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(pts: &[(f64, f64)]) -> LineString<f64> {
        LineString::new(pts.iter().map(|&(x, y)| Coord { x, y }).collect())
    }

    fn raster() -> ContourRaster {
        ContourRaster::new(Coord { x: 0.0, y: 0.0 }, 1.0, 20, 20)
    }

    #[test]
    fn write_contour_is_a_no_op_for_the_same_value() {
        let mut r = raster();
        let line = ls(&[(1.5, 1.5), (5.5, 1.5)]);
        r.write_contour(0, &line).unwrap();
        assert!(r.write_contour(0, &line).is_ok());
    }

    #[test]
    fn write_contour_crashes_on_conflict() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)])).unwrap();
        let err = r.write_contour(1, &ls(&[(3.5, 1.5), (3.5, 5.5)]));
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("rasterization_px_size"));
    }

    #[test]
    fn find_conflicts_reports_the_pixel_and_contour_without_writing_anything() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)])).unwrap();
        let conflicts = r.find_conflicts(1, &ls(&[(3.5, 1.5), (3.5, 5.5)]));
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0], (r.pixel_center(3, 1), 0));
        // Nothing was actually written under contour 1's index.
        assert_eq!(r.get(3, 1), 1);
    }

    #[test]
    fn find_conflicts_is_empty_when_the_line_can_be_written_cleanly() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)])).unwrap();
        assert!(r
            .find_conflicts(1, &ls(&[(1.5, 10.5), (5.5, 10.5)]))
            .is_empty());
    }

    #[test]
    fn find_conflicts_reports_every_conflicting_pixel_along_a_parallel_run() {
        // Two long parallel lines, close enough together to land in the same
        // pixel row along their whole length: every pixel one touches
        // conflicts with the other, not just the first -- this is what lets
        // the caller tell this genuinely-crowded case apart from a small
        // conflict cluster right at a digitizing gap.
        let mut r = raster();
        r.write_contour(0, &ls(&[(0.5, 1.4), (19.5, 1.4)])).unwrap();
        let conflicts = r.find_conflicts(1, &ls(&[(0.5, 1.6), (19.5, 1.6)]));
        assert!(conflicts.len() > 10, "{}", conflicts.len());
        assert!(conflicts.iter().all(|&(_, idx)| idx == 0));
    }

    #[test]
    fn write_contour_supercover_finds_the_shared_corner_pixel() {
        // A single-pixel-wide diagonal contour: pixel (2,2) then (3,3). A
        // plain Bresenham walk can skip the diagonally-adjacent corner pixel
        // between them; the doc's whole point in specifying Supercover is
        // that it must not be skipped here.
        let mut r = raster();
        r.write_contour(0, &ls(&[(2.5, 2.5), (3.5, 3.5)])).unwrap();
        // Every pixel a plain diagonal step could ambiguously touch around
        // the crossing must have been written.
        assert_ne!(r.get(2, 2), 0);
        assert_ne!(r.get(3, 3), 0);
    }

    #[test]
    fn first_hit_excludes_the_source_contour() {
        let mut r = raster();
        // Contour 0 stops short of contour 1, so they never physically cross.
        r.write_contour(0, &ls(&[(0.5, 5.5), (8.5, 5.5)])).unwrap();
        r.write_contour(1, &ls(&[(15.5, 0.5), (15.5, 19.5)]))
            .unwrap();
        // Stepping along contour 0's own pixels must not "hit" itself.
        assert_eq!(
            r.first_hit_along_step(Coord { x: 1.0, y: 5.5 }, Coord { x: 2.0, y: 5.5 }, 0),
            None
        );
        // A foreign contour further along the same row is still found.
        assert_eq!(
            r.first_hit_along_step(Coord { x: 14.0, y: 5.5 }, Coord { x: 17.0, y: 5.5 }, 0),
            Some(1)
        );
    }

    #[test]
    fn first_hit_along_step_does_not_miss_a_pixel_a_long_shallow_step_only_clips() {
        // A long (~7.5 unit), shallow-angle step whose two endpoints' own
        // pixels are nowhere near (3, 1), even though the straight line
        // between them clips it briefly partway through. Converting `prev`
        // and `next` to pixel indices first and walking Supercover directly
        // between *those* -- Appendix 5's own reference approach -- misses
        // it, since neither endpoint's own pixel is anywhere close; found
        // by sweeping random long segments against a dense, independent
        // sample of the same line until one exposed the gap.
        let mut r = raster();
        r.write_contour(0, &ls(&[(3.5, 1.5), (3.6, 1.5)])).unwrap();

        let prev = Coord { x: 2.29, y: 0.77 };
        let next = Coord {
            x: 9.51575404240981,
            y: 2.3181153434354256,
        };
        assert_eq!(r.first_hit_along_step(prev, next, u64::MAX), Some(0));
    }

    #[test]
    fn first_hit_along_step_finds_a_flanking_pixel_at_an_exact_diagonal_corner_crossing() {
        // A perfect 45-degree step passes exactly through the shared corner
        // of four pixels at (3, 3). Both pixels flanking that corner --
        // (3, 2) here, not just the two the step's own line runs through --
        // count as touched: the same conservative convention Supercover
        // itself uses for a corner-crossing diagonal step, so a thin
        // contour placed exactly at a corner is never skipped either.
        let mut r = raster();
        r.write_contour(0, &ls(&[(3.5, 2.5), (3.6, 2.5)])).unwrap();

        let prev = Coord { x: 2.5, y: 2.5 };
        let next = Coord { x: 4.5, y: 4.5 };
        assert_eq!(r.first_hit_along_step(prev, next, u64::MAX), Some(0));
    }

    #[test]
    fn nearest_contour_within_radius_finds_a_pixel_not_exactly_under_pos() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(5.5, 5.5), (5.5, 15.5)])).unwrap();
        // 2m off the contour's own pixel column, within a 3m radius.
        let pos = Coord { x: 7.5, y: 10.5 };
        assert_eq!(r.nearest_contour_within_radius(pos, 3.0), Some(0));
    }

    #[test]
    fn nearest_contour_within_radius_is_none_when_nothing_is_close_enough() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(5.5, 5.5), (5.5, 15.5)])).unwrap();
        let pos = Coord { x: 15.5, y: 10.5 }; // 10m away
        assert_eq!(r.nearest_contour_within_radius(pos, 3.0), None);
    }

    #[test]
    fn nearest_contour_within_radius_picks_the_closer_of_two_contours() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(2.5, 0.5), (2.5, 19.5)])).unwrap();
        r.write_contour(1, &ls(&[(8.5, 0.5), (8.5, 19.5)])).unwrap();
        // 2m from contour 0's own pixel column, 4m from contour 1's.
        let pos = Coord { x: 4.5, y: 10.5 };
        assert_eq!(r.nearest_contour_within_radius(pos, 5.0), Some(0));
    }

    #[test]
    fn pixels_in_polygon_grid_aligned_square() {
        let r = raster();
        let square = Polygon::new(
            ls(&[(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0), (2.0, 2.0)]),
            vec![],
        );
        let hits = r.pixels_in_polygon(&square);
        assert_eq!(hits.len(), 9); // pixels (2..=4, 2..=4)
        assert!(hits.contains(&(2, 2)));
        assert!(hits.contains(&(4, 4)));
        assert!(!hits.contains(&(5, 5)));
    }
}
