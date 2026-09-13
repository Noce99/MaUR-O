//! The Contour Raster (`Contours-to-Raster.md`, Step 1): a 2D grid recording,
//! per pixel, one of four reserved values or a shifted contour index, plus
//! Appendix 4's pixel-walk used both to fill it and to find contour
//! intersections in Steps 1 and 3.

use std::path::Path;

use geo::{Coord, LineString, Polygon};

/// Nothing has claimed this pixel yet.
pub const UNDEFINED: u32 = 0;
/// Computed by [`ContourRaster::compute_out_of_bound`].
pub const OUT_OF_BOUND: u32 = 1;
/// Reached by a Hot Rain Drop Production's flood fill (Step 1) but not on
/// any contour.
pub const NO_CONTOUR_IN_BOUND: u32 = 2;
/// Two or more contours conflicted here, or a Jump's own buffered polygon
/// covers it (see [`ContourRaster::write_contour`],
/// [`ContourRaster::mark_high_density_polygon`]).
pub const HIGH_DENSITY: u32 = 3;
/// A Flying End's own not-yet-final tail, one grow step at a time (Step 1's
/// Growing Process, case (c)): while a Flying End is still flying, its own
/// growth so far isn't drawn under its real contour index (that only
/// happens once it resolves -- [`ContourRaster::write_contour`]), but it
/// still needs to act as a repeller for every *other* Flying End growing
/// alongside it, or two contours seeking the border independently, close
/// and parallel, can cross one another unnoticed. Set by
/// [`ContourRaster::mark_temporary_step`], read the same as any real
/// contour pixel by the Growing Process's own window scan, and swept back
/// to `NO_CONTOUR_IN_BOUND` by [`ContourRaster::clear_temporary_contours`]
/// once the whole Growing Process (both passes, every Flying End resolved)
/// finishes -- not per contour as it resolves, since a still-flying
/// neighbor may still need it as a repeller.
pub const TEMPORARY_CONTOUR: u32 = 4;
/// A raster value at or above this means "contour (value -
/// `CONTOUR_0_MATRIX_VALUE`)". Kept as a single named constant, not a
/// config-file parameter, so a future shift in the five reserved values
/// above only has to change this one place (see `Contours-to-Raster.md`'s
/// own note on it, Step 1).
pub const CONTOUR_0_MATRIX_VALUE: u32 = 5;

/// A grid recording, per pixel, one of the four reserved values above, or
/// `CONTOUR_0_MATRIX_VALUE + i` for the contour at index `i`.
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

/// What the first non-clear pixel along a stepped segment turned out to be.
/// See Appendix 4 and the Rain Drop Production Definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepHit {
    /// The contour index hit (excluding the caller's own excluded contour).
    Contour(u64),
    /// An out-of-bound (`1`) pixel, including stepping off the raster array.
    OutOfBound,
    /// A high-density (`3`) pixel.
    HighDensity,
}

impl ContourRaster {
    /// An all-`UNDEFINED` raster of `width` x `height` pixels, `px_size`
    /// meters each, with `origin` as the ground-meter position of pixel
    /// `(0, 0)`.
    pub fn new(origin: Coord<f64>, px_size: f64, width: usize, height: usize) -> ContourRaster {
        ContourRaster {
            grid: vec![vec![UNDEFINED; width]; height],
            origin,
            px_size,
            width,
            height,
        }
    }

    /// Converts a world-space Coord to raster pixel indices, using this
    /// raster's own origin and pixel size. See Appendix 4.
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

    /// The raw pixel value, or `OUT_OF_BOUND` for a pixel outside the raster
    /// -- leaving the array is, semantically, exactly what `OUT_OF_BOUND`
    /// already means, so callers don't need a separate bounding-box check
    /// before stepping (see Appendix 4).
    pub fn get(&self, x: i64, y: i64) -> u32 {
        if x < 0 || y < 0 {
            return OUT_OF_BOUND;
        }
        let (x, y) = (x as usize, y as usize);
        self.grid
            .get(y)
            .and_then(|row| row.get(x))
            .copied()
            .unwrap_or(OUT_OF_BOUND)
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

    /// Writes `contour_idx` (as `CONTOUR_0_MATRIX_VALUE + contour_idx`) to
    /// every pixel `ls` touches, walking each consecutive pair of points
    /// with Appendix 4's continuous pixel-walk so every cell the line
    /// geometrically touches is found -- including a diagonally-crossed
    /// shared corner pixel -- regardless of how long a segment is. A pixel
    /// that's `UNDEFINED`, `NO_CONTOUR_IN_BOUND`, or `TEMPORARY_CONTOUR` is
    /// free to claim (the last of those is exactly this contour's own
    /// tentative trail from Step 1's Growing Process, case (c), becoming
    /// real as this contour resolves -- see
    /// [`Self::mark_temporary_step`]); a pixel that already holds this same
    /// contour's value is a no-op; a pixel that's `OUT_OF_BOUND` is left
    /// alone -- it is the map's own edge, not another contour, and Step 1's
    /// Growing Process routinely walks a contour's very last segment right
    /// up to one on purpose (case (b)'s snap, or case (c) landing on one
    /// directly), so this is the intended, successful way for a contour to
    /// end, not a conflict to flag; any other existing value (another
    /// contour's, or already `HIGH_DENSITY`) means the pixel becomes
    /// `HIGH_DENSITY` instead -- there is no crash path any more (Step 1).
    pub fn write_contour(&mut self, contour_idx: u64, ls: &LineString<f64>) {
        let value = CONTOUR_0_MATRIX_VALUE + contour_idx as u32;
        let pts = &ls.0;
        if pts.len() < 2 {
            return;
        }
        let (origin, px_size) = (self.origin, self.px_size);
        for w in pts.windows(2) {
            walk_pixels(origin, px_size, w[0], w[1], |x, y| {
                let current = self.get(x, y);
                if current != value && current != OUT_OF_BOUND {
                    if current == UNDEFINED
                        || current == NO_CONTOUR_IN_BOUND
                        || current == TEMPORARY_CONTOUR
                    {
                        self.set(x, y, value);
                    } else {
                        self.set(x, y, HIGH_DENSITY);
                    }
                }
                true
            });
        }
    }

    /// Marks every pixel walked between `prev` and `next` -- one Growing
    /// Process step's worth of new, not-yet-final path (case (c)) -- as
    /// `TEMPORARY_CONTOUR`, so it acts as a repeller
    /// (`WindowPixelKind::Contour` in `step1_extract`'s own window scan) for
    /// every other Flying End still growing, even though this Flying End's
    /// contour hasn't resolved yet and so can't claim it under its real
    /// index. Only `UNDEFINED` and `NO_CONTOUR_IN_BOUND` pixels are
    /// overwritten; an `OUT_OF_BOUND`/`HIGH_DENSITY` pixel (a genuine,
    /// successful landing spot, not a conflict) and a real contour's own
    /// pixel (whatever conflict that may or may not turn out to be is
    /// caught for real once this contour resolves, via
    /// [`Self::write_contour`]) are both left exactly as they are.
    pub fn mark_temporary_step(&mut self, prev: Coord<f64>, next: Coord<f64>) {
        let (origin, px_size) = (self.origin, self.px_size);
        walk_pixels(origin, px_size, prev, next, |x, y| {
            let current = self.get(x, y);
            if current == UNDEFINED || current == NO_CONTOUR_IN_BOUND {
                self.set(x, y, TEMPORARY_CONTOUR);
            }
            true
        });
    }

    /// Resets every remaining `TEMPORARY_CONTOUR` pixel to
    /// `NO_CONTOUR_IN_BOUND`. Called once the whole Growing Process (both
    /// passes, every Flying End resolved) finishes: a Flying End's own final
    /// `ls` claims its temporary trail for real, under its own contour
    /// index, as it resolves ([`Self::write_contour`]), so whatever is still
    /// `TEMPORARY_CONTOUR` at that point is a stretch of path some Flying
    /// End tried and then abandoned in favor of a different final route
    /// (e.g. resampling after a merge or a close moved its nodes elsewhere).
    /// Deliberately not run per contour as each one resolves -- a
    /// still-flying neighbor may still be relying on that same pixel as a
    /// repeller.
    pub fn clear_temporary_contours(&mut self) {
        for row in &mut self.grid {
            for cell in row.iter_mut() {
                if *cell == TEMPORARY_CONTOUR {
                    *cell = NO_CONTOUR_IN_BOUND;
                }
            }
        }
    }

    /// After merging contour `remove_idx`'s data into contour `keep_idx`
    /// (`keep_idx < remove_idx`), repoints every pixel holding
    /// `remove_idx`'s value to `keep_idx`'s, then shifts every pixel with a
    /// higher index down by one to close the gap left in the Contours
    /// vector -- Step 1's Growing Process, case (a).
    pub fn merge_contour_indices(&mut self, keep_idx: u64, remove_idx: u64) {
        let remove_val = CONTOUR_0_MATRIX_VALUE + remove_idx as u32;
        let keep_val = CONTOUR_0_MATRIX_VALUE + keep_idx as u32;
        for row in &mut self.grid {
            for cell in row.iter_mut() {
                if *cell == remove_val {
                    *cell = keep_val;
                } else if *cell > remove_val {
                    *cell -= 1;
                }
            }
        }
    }

    /// Resets every pixel currently holding exactly `contour_idx`'s own
    /// value back to `NO_CONTOUR_IN_BOUND`, so its whole footprint can be
    /// redrawn fresh from a newly resampled `ls` without leaving behind
    /// pixels from wherever it used to run before the resample moved it.
    /// Needed whenever Step 1's Growing Process resamples a contour that was
    /// already (at least partly) drawn -- closing it into a ring re-derives
    /// every node against a perimeter-adjusted step, and merging splices in
    /// a new joint segment of whatever length the two Flying Ends happened
    /// to leave -- either can shift already-drawn nodes out of phase with
    /// the pixels their old positions already claimed, not just add new
    /// ones. Every caller of this runs after Step 1's own flood-fill and
    /// out-of-bound computation are already done, so a pixel losing its
    /// contour here is, as far as either of those already found, in bound
    /// and on no contour -- `NO_CONTOUR_IN_BOUND`, not `UNDEFINED`, which is
    /// reserved for a pixel neither of those two passes, nor any contour,
    /// ever claimed in the first place (a sealed-off pocket, see
    /// `compute_out_of_bound`'s own doc comment); resetting to `UNDEFINED`
    /// instead would misreport an already-classified pixel as one neither
    /// pass ever reached. A pixel already `HIGH_DENSITY` is left alone: some
    /// other contour might share it too, and there is no way to tell from
    /// the pixel's own value alone, so the conservative "don't trust it"
    /// state stays rather than risk erasing evidence that still belongs
    /// there.
    pub fn clear_contour(&mut self, contour_idx: u64) {
        let value = CONTOUR_0_MATRIX_VALUE + contour_idx as u32;
        for row in &mut self.grid {
            for cell in row.iter_mut() {
                if *cell == value {
                    *cell = NO_CONTOUR_IN_BOUND;
                }
            }
        }
    }

    /// Sets every pixel whose center falls inside `poly` to `HIGH_DENSITY`,
    /// independently of whatever value it held before (a Jump's own area,
    /// Step 1).
    pub fn mark_high_density_polygon(&mut self, poly: &Polygon<f64>) {
        for (x, y) in self.pixels_in_polygon(poly) {
            self.set(x, y, HIGH_DENSITY);
        }
    }

    /// Computes the out-of-bound (`1`) area (Step 1): every border pixel
    /// starts `OUT_OF_BOUND`; that's flooded 8-connectedly through every
    /// `UNDEFINED` pixel reachable from it (a contour/high-density/
    /// no-contour-in-bound pixel blocks the flood, which is exactly how a
    /// fully enclosed `UNDEFINED` pocket can legitimately survive); then
    /// `extra_dilation` further 8-connected rounds grow `OUT_OF_BOUND` over
    /// *any* value, not just `UNDEFINED`.
    pub fn compute_out_of_bound(&mut self, extra_dilation: usize) {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return;
        }
        let mut active: Vec<(usize, usize)> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if x == 0 || x == w - 1 || y == 0 || y == h - 1 {
                    self.grid[y][x] = OUT_OF_BOUND;
                    active.push((x, y));
                }
            }
        }
        loop {
            let mut next_active = Vec::new();
            for &(x, y) in &active {
                for (nx, ny) in Self::neighbors8(x, y, w, h) {
                    if self.grid[ny][nx] == UNDEFINED {
                        self.grid[ny][nx] = OUT_OF_BOUND;
                        next_active.push((nx, ny));
                    }
                }
            }
            if next_active.is_empty() {
                break;
            }
            active = next_active;
        }
        for _ in 0..extra_dilation {
            if active.is_empty() {
                break;
            }
            let mut next_active = Vec::new();
            for &(x, y) in &active {
                for (nx, ny) in Self::neighbors8(x, y, w, h) {
                    if self.grid[ny][nx] != OUT_OF_BOUND {
                        self.grid[ny][nx] = OUT_OF_BOUND;
                        next_active.push((nx, ny));
                    }
                }
            }
            active = next_active;
        }
    }

    fn neighbors8(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = (usize, usize)> {
        let (xi, yi) = (x as i64, y as i64);
        let (wi, hi) = (w as i64, h as i64);
        (-1i64..=1).flat_map(move |dy| {
            (-1i64..=1).filter_map(move |dx| {
                if dx == 0 && dy == 0 {
                    return None;
                }
                let (nx, ny) = (xi + dx, yi + dy);
                (nx >= 0 && ny >= 0 && nx < wi && ny < hi).then_some((nx as usize, ny as usize))
            })
        })
    }

    /// Walks every pixel the segment `prev -> next` traverses and returns
    /// the first of an out-of-bound pixel, a high-density pixel, or a
    /// contour other than `exclude_contour_idx` (a rain drop's own source
    /// contour), whichever comes first -- or `None` if the step is clear.
    /// See Appendix 4 and the Rain Drop Production Definition.
    pub fn first_hit_along_step(
        &self,
        prev: Coord<f64>,
        next: Coord<f64>,
        exclude_contour_idx: u64,
    ) -> Option<StepHit> {
        let mut hit = None;
        walk_pixels(self.origin, self.px_size, prev, next, |x, y| {
            let val = self.get(x, y);
            if val == OUT_OF_BOUND {
                hit = Some(StepHit::OutOfBound);
                return false;
            }
            if val == HIGH_DENSITY {
                hit = Some(StepHit::HighDensity);
                return false;
            }
            if val < CONTOUR_0_MATRIX_VALUE {
                return true; // undefined or no-contour-in-bound: keep walking
            }
            let contour_idx = (val - CONTOUR_0_MATRIX_VALUE) as u64;
            if contour_idx == exclude_contour_idx {
                return true;
            }
            hit = Some(StepHit::Contour(contour_idx));
            false // found one, stop
        });
        hit
    }

    /// Like [`Self::first_hit_along_step`], but also returns every
    /// `UNDEFINED` pixel visited before the hit (or before reaching `next`,
    /// if the step is clear) -- candidates for Step 1's flood-fill.
    /// Non-mutating on purpose: a Hot rain drop only knows whether its whole
    /// path is safe to commit (via [`Self::commit_flood_pixels`]) once it
    /// has actually evaporated -- see that method's own doc comment for why.
    pub fn step_flood_candidates(
        &self,
        prev: Coord<f64>,
        next: Coord<f64>,
        exclude_contour_idx: u64,
    ) -> (Option<StepHit>, Vec<(i64, i64)>) {
        let mut hit = None;
        let mut candidates = Vec::new();
        walk_pixels(self.origin, self.px_size, prev, next, |x, y| {
            let val = self.get(x, y);
            if val == OUT_OF_BOUND {
                hit = Some(StepHit::OutOfBound);
                return false;
            }
            if val == HIGH_DENSITY {
                hit = Some(StepHit::HighDensity);
                return false;
            }
            if val < CONTOUR_0_MATRIX_VALUE {
                if val == UNDEFINED {
                    candidates.push((x, y));
                }
                return true; // undefined or no-contour-in-bound: keep walking
            }
            let contour_idx = (val - CONTOUR_0_MATRIX_VALUE) as u64;
            if contour_idx == exclude_contour_idx {
                return true;
            }
            hit = Some(StepHit::Contour(contour_idx));
            false // found one, stop
        });
        (hit, candidates)
    }

    /// Sets every one of `pixels` still `UNDEFINED` to `NO_CONTOUR_IN_BOUND`
    /// -- Step 1's flood-fill, committing a Hot rain drop's own accumulated
    /// path once it's known to have evaporated by hitting a contour or high
    /// density. Deliberately never called for a drop that instead evaporates
    /// by leaving the map: committing that path's pixels would plant a
    /// firebreak of `NO_CONTOUR_IN_BOUND` pixels between the true out-of-
    /// bound area and the raster's own border, which the later
    /// [`Self::compute_out_of_bound`] flood-fill can only ever spread
    /// through `UNDEFINED` pixels -- so those pixels would be stuck showing
    /// "in bound" when they are really outside the map.
    pub fn commit_flood_pixels(&mut self, pixels: &[(i64, i64)]) {
        for &(x, y) in pixels {
            if self.get(x, y) == UNDEFINED {
                self.set(x, y, NO_CONTOUR_IN_BOUND);
            }
        }
    }
}

/// Calls `visit` with every pixel the segment `prev -> next` touches, in the
/// order visited, stopping early if `visit` returns `false`. A raster's
/// `origin` and `px_size` place the segment (given in world-space ground
/// meters) into pixel space. See Appendix 4.
///
/// A cheaper-looking alternative would convert `prev` and `next` to pixel
/// indices first and walk something like the `line_drawing` crate's
/// `Supercover` between *those two cells* -- but that only ever sees which
/// cell each endpoint floors into, never where within that cell it actually
/// sits, nor the continuous line's real path between them. A thin,
/// diagonally-placed contour can clip a multi-pixel-long segment for a
/// fraction of its length without either endpoint's own pixel being
/// anywhere near it, and that crossing would be missed entirely -- whether
/// the segment being walked is a rain drop's own step or a stretch of a
/// contour's own `ls` being written to the raster. This instead walks the
/// continuous segment directly in pixel space (a standard grid-traversal/
/// DDA walk, computing exactly where it crosses each pixel boundary), so
/// every cell the line geometrically touches is found regardless of how
/// long the segment is or where exactly within their own pixels the
/// endpoints fall.
fn walk_pixels(
    origin: Coord<f64>,
    px_size: f64,
    prev: Coord<f64>,
    next: Coord<f64>,
    mut visit: impl FnMut(i64, i64) -> bool,
) {
    let fx0 = (prev.x - origin.x) / px_size;
    let fy0 = (prev.y - origin.y) / px_size;
    let fx1 = (next.x - origin.x) / px_size;
    let fy1 = (next.y - origin.y) / px_size;
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

impl ContourRaster {
    /// The index of the contour whose nearest pixel (by pixel-center
    /// distance) to `pos` lies within `radius` ground meters, or `None` if
    /// no contour has one that close. A Slope Line's own position is not
    /// always pixel-exact on top of its contour, so Step 1 uses this
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
                if val < CONTOUR_0_MATRIX_VALUE {
                    continue;
                }
                let center = self.pixel_center(x, y);
                let dist = (center.x - pos.x).hypot(center.y - pos.y);
                if dist > radius {
                    continue;
                }
                if best.is_none_or(|(best_dist, _)| dist < best_dist) {
                    best = Some((dist, (val - CONTOUR_0_MATRIX_VALUE) as u64));
                }
            }
        }
        best.map(|(_, idx)| idx)
    }

    /// Every pixel (by index) whose center falls inside `poly`, found by
    /// scanning the polygon's own pixel-space bounding box. The doc gives no
    /// code for rasterizing an *area* -- Appendix 4's pixel walk only
    /// answers which pixels a *line* touches -- so this is new: cheap here
    /// since the polygons Step 2 rasterizes are thin buffered Jump lines,
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

    /// Writes this raster as a small-palette indexed PNG, one flat color per
    /// pixel value -- `UNDEFINED`, `OUT_OF_BOUND`, `NO_CONTOUR_IN_BOUND`,
    /// `HIGH_DENSITY`, or "a contour" (drawn the same regardless of index,
    /// same as the `--create_svg` vector output does). A full-raster
    /// companion to the vector SVGs, which (to keep file size sane) only
    /// draw a square for a contour or high-density pixel -- this covers the
    /// undefined/out-of-bound/no-contour-in-bound area those leave blank.
    pub fn write_png(&self, path: &Path) -> Result<(), String> {
        const LIGHT_GREY: [u8; 3] = [211, 211, 211]; // UNDEFINED
        const BLACK: [u8; 3] = [0, 0, 0]; // OUT_OF_BOUND
        const WHITE: [u8; 3] = [255, 255, 255]; // NO_CONTOUR_IN_BOUND
        const LIGHT_GREEN: [u8; 3] = [144, 238, 144]; // HIGH_DENSITY
        const BROWN: [u8; 3] = [139, 69, 19]; // any contour
        const PALETTE: [[u8; 3]; 5] = [LIGHT_GREY, BLACK, WHITE, LIGHT_GREEN, BROWN];

        let mut indices = Vec::with_capacity(self.width * self.height);
        for y in 0..self.height as i64 {
            for x in 0..self.width as i64 {
                let val = self.get(x, y);
                indices.push(if val < CONTOUR_0_MATRIX_VALUE {
                    val as u8
                } else {
                    4
                });
            }
        }

        let file = std::fs::File::create(path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            self.width as u32,
            self.height as u32,
        );
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(PALETTE.iter().flatten().copied().collect::<Vec<u8>>());
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        writer
            .write_image_data(&indices)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        writer
            .finish()
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
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
        r.write_contour(0, &line);
        r.write_contour(0, &line);
        assert_eq!(r.get(1, 1), CONTOUR_0_MATRIX_VALUE);
    }

    #[test]
    fn write_contour_conflict_becomes_high_density_instead_of_crashing() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)]));
        r.write_contour(1, &ls(&[(3.5, 1.5), (3.5, 5.5)]));
        assert_eq!(r.get(3, 1), HIGH_DENSITY);
        // Pixels each contour touched on its own, away from the conflict,
        // still hold that contour's own value.
        assert_eq!(r.get(1, 1), CONTOUR_0_MATRIX_VALUE);
        assert_eq!(r.get(3, 4), CONTOUR_0_MATRIX_VALUE + 1);
    }

    #[test]
    fn write_contour_leaves_an_out_of_bound_pixel_out_of_bound() {
        // A contour's own last segment routinely runs right up to (and
        // through) an out-of-bound pixel on purpose -- Step 1's Growing
        // Process case (b) snaps a new node exactly onto one. That is a
        // successful ending, not a conflict with some other contour: the
        // pixel must stay `OUT_OF_BOUND`, never become `HIGH_DENSITY` or get
        // overwritten with this contour's own value.
        let mut r = raster();
        r.grid[1][3] = OUT_OF_BOUND;
        r.write_contour(0, &ls(&[(1.5, 1.5), (3.5, 1.5)]));
        assert_eq!(r.get(3, 1), OUT_OF_BOUND);
        // The rest of the same segment, away from the out-of-bound pixel,
        // still claims normally.
        assert_eq!(r.get(1, 1), CONTOUR_0_MATRIX_VALUE);
    }

    #[test]
    fn mark_temporary_step_only_claims_undefined_and_no_contour_in_bound_pixels() {
        let mut r = raster();
        r.grid[1][1] = NO_CONTOUR_IN_BOUND;
        r.grid[1][3] = OUT_OF_BOUND;
        r.grid[1][5] = HIGH_DENSITY;
        r.write_contour(0, &ls(&[(7.5, 1.5), (7.6, 1.5)])); // a real contour pixel at (7, 1)
        r.mark_temporary_step(Coord { x: 0.5, y: 1.5 }, Coord { x: 8.5, y: 1.5 });
        assert_eq!(r.get(0, 1), TEMPORARY_CONTOUR); // was UNDEFINED
        assert_eq!(r.get(1, 1), TEMPORARY_CONTOUR); // was NO_CONTOUR_IN_BOUND
        assert_eq!(r.get(3, 1), OUT_OF_BOUND); // left alone
        assert_eq!(r.get(5, 1), HIGH_DENSITY); // left alone
        assert_eq!(r.get(7, 1), CONTOUR_0_MATRIX_VALUE); // left alone
    }

    #[test]
    fn write_contour_claims_its_own_temporary_trail_without_becoming_high_density() {
        // Exactly what Step 1's Growing Process does as a Flying End
        // resolves: its own not-yet-final tail was marked `TEMPORARY_CONTOUR`
        // one step at a time as it grew (`mark_temporary_step`); the final
        // `write_contour` call that draws its whole, now-final `ls` in one
        // shot must claim that trail as this contour's own real value, not
        // treat it as a conflict with some other contour.
        let mut r = raster();
        r.mark_temporary_step(Coord { x: 1.5, y: 1.5 }, Coord { x: 5.5, y: 1.5 });
        assert_eq!(r.get(3, 1), TEMPORARY_CONTOUR);
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)]));
        assert_eq!(r.get(3, 1), CONTOUR_0_MATRIX_VALUE);
    }

    #[test]
    fn clear_temporary_contours_resets_remaining_pixels_to_no_contour_in_bound() {
        let mut r = raster();
        r.mark_temporary_step(Coord { x: 1.5, y: 1.5 }, Coord { x: 5.5, y: 1.5 });
        r.clear_temporary_contours();
        assert_eq!(r.get(1, 1), NO_CONTOUR_IN_BOUND);
        assert_eq!(r.get(5, 1), NO_CONTOUR_IN_BOUND);
    }

    #[test]
    fn clear_temporary_contours_leaves_every_other_value_alone() {
        let mut r = raster();
        r.grid[1][1] = OUT_OF_BOUND;
        r.grid[1][3] = HIGH_DENSITY;
        r.write_contour(0, &ls(&[(5.5, 1.5), (5.6, 1.5)]));
        r.clear_temporary_contours();
        assert_eq!(r.get(1, 1), OUT_OF_BOUND);
        assert_eq!(r.get(3, 1), HIGH_DENSITY);
        assert_eq!(r.get(5, 1), CONTOUR_0_MATRIX_VALUE);
        assert_eq!(r.get(0, 0), UNDEFINED);
    }

    #[test]
    fn clear_contour_resets_its_pixels_to_no_contour_in_bound_not_undefined() {
        // As every real caller (Step 1's Growing Process) finds the raster:
        // Step 1's own flood-fill and out-of-bound computation have already
        // run, so a pixel losing its contour here is, as far as either of
        // those already found, known to be in bound and on no contour --
        // not "undefined" (reserved for a pixel neither pass, nor any
        // contour, ever claimed at all).
        let mut r = raster();
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)]));
        r.clear_contour(0);
        assert_eq!(r.get(1, 1), NO_CONTOUR_IN_BOUND);
        assert_eq!(r.get(5, 1), NO_CONTOUR_IN_BOUND);
    }

    #[test]
    fn clear_contour_leaves_a_high_density_pixel_alone() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(1.5, 1.5), (5.5, 1.5)]));
        r.write_contour(1, &ls(&[(3.5, 1.5), (3.5, 5.5)]));
        r.clear_contour(0);
        // (3, 1) was HIGH_DENSITY, not contour 0's own value -- clearing
        // contour 0 must not touch it, since contour 1 might still be there.
        assert_eq!(r.get(3, 1), HIGH_DENSITY);
    }

    #[test]
    fn write_contour_claims_a_no_contour_in_bound_pixel_without_conflict() {
        let mut r = raster();
        // As the Step 1 flood-fill would leave it: reached, but on no contour.
        r.grid[1][1] = NO_CONTOUR_IN_BOUND;
        r.write_contour(0, &ls(&[(1.5, 1.5), (1.6, 1.5)]));
        assert_eq!(r.get(1, 1), CONTOUR_0_MATRIX_VALUE);
    }

    #[test]
    fn write_contour_finds_the_shared_corner_pixel() {
        // A single-pixel-wide diagonal contour: pixel (2,2) then (3,3). A
        // plain Bresenham walk can skip the diagonally-adjacent corner pixel
        // between them; the doc's whole point in specifying this pixel walk
        // is that it must not be skipped here.
        let mut r = raster();
        r.write_contour(0, &ls(&[(2.5, 2.5), (3.5, 3.5)]));
        // Every pixel a plain diagonal step could ambiguously touch around
        // the crossing must have been written.
        assert_ne!(r.get(2, 2), UNDEFINED);
        assert_ne!(r.get(3, 3), UNDEFINED);
    }

    #[test]
    fn first_hit_excludes_the_source_contour() {
        let mut r = raster();
        // Contour 0 stops short of contour 1, so they never physically cross.
        r.write_contour(0, &ls(&[(0.5, 5.5), (8.5, 5.5)]));
        r.write_contour(1, &ls(&[(15.5, 0.5), (15.5, 19.5)]));
        // Stepping along contour 0's own pixels must not "hit" itself.
        assert_eq!(
            r.first_hit_along_step(Coord { x: 1.0, y: 5.5 }, Coord { x: 2.0, y: 5.5 }, 0),
            None
        );
        // A foreign contour further along the same row is still found.
        assert_eq!(
            r.first_hit_along_step(Coord { x: 14.0, y: 5.5 }, Coord { x: 17.0, y: 5.5 }, 0),
            Some(StepHit::Contour(1))
        );
    }

    #[test]
    fn first_hit_along_step_does_not_miss_a_pixel_a_long_shallow_step_only_clips() {
        // A long (~7.5 unit), shallow-angle step whose two endpoints' own
        // pixels are nowhere near (3, 1), even though the straight line
        // between them clips it briefly partway through. Converting `prev`
        // and `next` to pixel indices first and walking Supercover directly
        // between *those* -- the naive alternative Appendix 4 rejects -- misses
        // it, since neither endpoint's own pixel is anywhere close; found
        // by sweeping random long segments against a dense, independent
        // sample of the same line until one exposed the gap.
        let mut r = raster();
        r.write_contour(0, &ls(&[(3.5, 1.5), (3.6, 1.5)]));

        let prev = Coord { x: 2.29, y: 0.77 };
        let next = Coord {
            x: 9.51575404240981,
            y: 2.3181153434354256,
        };
        assert_eq!(
            r.first_hit_along_step(prev, next, u64::MAX),
            Some(StepHit::Contour(0))
        );
    }

    #[test]
    fn first_hit_along_step_finds_a_flanking_pixel_at_an_exact_diagonal_corner_crossing() {
        // A perfect 45-degree step passes exactly through the shared corner
        // of four pixels at (3, 3). Both pixels flanking that corner --
        // (3, 2) here, not just the two the step's own line runs through --
        // count as touched: the same conservative convention this pixel
        // walk uses for a corner-crossing diagonal step, so a thin contour
        // placed exactly at a corner is never skipped either.
        let mut r = raster();
        r.write_contour(0, &ls(&[(3.5, 2.5), (3.6, 2.5)]));

        let prev = Coord { x: 2.5, y: 2.5 };
        let next = Coord { x: 4.5, y: 4.5 };
        assert_eq!(
            r.first_hit_along_step(prev, next, u64::MAX),
            Some(StepHit::Contour(0))
        );
    }

    #[test]
    fn first_hit_along_step_reports_out_of_bound_off_the_array() {
        let r = raster();
        assert_eq!(
            r.first_hit_along_step(
                Coord { x: 0.5, y: 0.5 },
                Coord { x: -5.0, y: 0.5 },
                u64::MAX
            ),
            Some(StepHit::OutOfBound)
        );
    }

    #[test]
    fn first_hit_along_step_reports_high_density() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(3.5, 1.5), (3.5, 1.5001)]));
        r.write_contour(1, &ls(&[(3.5, 1.5), (3.5, 1.5001)])); // same pixel, different contour
        assert_eq!(r.get(3, 1), HIGH_DENSITY);
        assert_eq!(
            r.first_hit_along_step(Coord { x: 0.5, y: 1.5 }, Coord { x: 6.5, y: 1.5 }, u64::MAX),
            Some(StepHit::HighDensity)
        );
    }

    #[test]
    fn nearest_contour_within_radius_finds_a_pixel_not_exactly_under_pos() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(5.5, 5.5), (5.5, 15.5)]));
        // 2m off the contour's own pixel column, within a 3m radius.
        let pos = Coord { x: 7.5, y: 10.5 };
        assert_eq!(r.nearest_contour_within_radius(pos, 3.0), Some(0));
    }

    #[test]
    fn nearest_contour_within_radius_is_none_when_nothing_is_close_enough() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(5.5, 5.5), (5.5, 15.5)]));
        let pos = Coord { x: 15.5, y: 10.5 }; // 10m away
        assert_eq!(r.nearest_contour_within_radius(pos, 3.0), None);
    }

    #[test]
    fn nearest_contour_within_radius_picks_the_closer_of_two_contours() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(2.5, 0.5), (2.5, 19.5)]));
        r.write_contour(1, &ls(&[(8.5, 0.5), (8.5, 19.5)]));
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

    #[test]
    fn mark_high_density_polygon_overwrites_any_previous_value() {
        let mut r = raster();
        r.write_contour(0, &ls(&[(2.5, 2.5), (2.6, 2.5)]));
        let square = Polygon::new(
            ls(&[(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0), (2.0, 2.0)]),
            vec![],
        );
        r.mark_high_density_polygon(&square);
        assert_eq!(r.get(2, 2), HIGH_DENSITY); // was a contour pixel
        assert_eq!(r.get(4, 4), HIGH_DENSITY); // was undefined
    }

    #[test]
    fn compute_out_of_bound_marks_the_border_and_floods_inward() {
        let mut r = raster();
        r.compute_out_of_bound(0);
        assert_eq!(r.get(0, 0), OUT_OF_BOUND);
        assert_eq!(r.get(19, 19), OUT_OF_BOUND);
        // Nothing blocks the flood on an otherwise-empty raster: every
        // pixel ends up out of bound.
        assert_eq!(r.get(10, 10), OUT_OF_BOUND);
    }

    #[test]
    fn compute_out_of_bound_is_blocked_by_a_ring_of_contour_pixels() {
        let mut r = ContourRaster::new(Coord { x: 0.0, y: 0.0 }, 1.0, 11, 11);
        // A closed ring a couple of pixels in from the border.
        r.write_contour(
            0,
            &ls(&[(2.5, 2.5), (8.5, 2.5), (8.5, 8.5), (2.5, 8.5), (2.5, 2.5)]),
        );
        r.compute_out_of_bound(0);
        assert_eq!(r.get(0, 0), OUT_OF_BOUND);
        // Enclosed and never reached: stays undefined.
        assert_eq!(r.get(5, 5), UNDEFINED);
    }

    #[test]
    fn compute_out_of_bound_extra_dilation_eats_into_other_values() {
        let mut r = ContourRaster::new(Coord { x: 0.0, y: 0.0 }, 1.0, 11, 11);
        r.write_contour(0, &ls(&[(0.5, 5.5), (1.5, 5.5)])); // right on the border
        r.compute_out_of_bound(2);
        // The extra dilation rounds should have overwritten that contour
        // pixel too, since it sits within 2 pixels of the border.
        assert_eq!(r.get(0, 5), OUT_OF_BOUND);
    }
}
