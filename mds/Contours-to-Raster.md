- This is an algorithm description that should, given some contours and other terrain objects from a .omap, return a raster of the given map with, for each pixel, the elevation value up to a constant with respect to the real ones.
- ## Assumptions
  collapsed:: true
	- 1) This algorithm assumes that the gravity of the same contour line is always going in the same direction. In other words, it is not possible to find a contour line with slope lines pointing toward different parts of the same contour line.
	- The form lines are completely ignored
- ## Parameters
	- **bezier_linearization_step** (meters) — maximum chord length used to flatten a cubic Bezier segment into straight lines ([Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)). Should be noticeably smaller than **contours_step**: it runs first, and the curve detail it captures is exactly what the equal-chord resampling with **contours_step** could otherwise erase from a curve if the two were comparable.
	- **contours_step** (meters) — the equally-spaced node distance every contour's final `ls` is resampled to ([Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)). This is the working resolution the rest of the algorithm (source placement, circle fitting, and the Growing Process's own step size and window scale — see Step 1) operates on.
	- **rasterization_px_size** (meters) — pixel size of the Contour Raster (Step 1). The finest positional resolution the whole algorithm can resolve; it must be small enough that no two genuinely distinct contours ever land in the same pixel, since a conflict there is marked as high density rather than resolved to either contour — in practice it is bounded by the tightest contour bunching expected on the map (e.g. at cliffs).
	- **heavy_object_width** (meters, `width` in [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e)) — buffer width used to turn a **Jump**'s or a **Heavy Object**'s own `ls` into a polygon. For a Jump, this is the area Step 2 scans to find which contours it touches; for a Heavy Object, it is the area Step 1 itself searches for an intersecting contour, instead of only the pixels directly under its digitized line. Should be at least a couple of **rasterization_px_size**, so the buffered polygon covers a real ring of pixels rather than collapsing into a single pixel row.
	- **heavy_object_growing** (meters, `extra_growing` in [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e)) — extra padding buffered on top of **heavy_object_width**. Normally much smaller than **heavy_object_width** itself — a small safety margin, not a second width.
	- **circumference_fitting_points_number** (pure number) — how many **Coord**s on each side of a Heavy Object/contour intersection are used to fit a circle (Step 1). Should be a small multiple of the point density set by **contours_step**: enough points for a stable fit (at least 3), but few enough that the sampled arc still reflects local curvature at the intersection rather than the contour's shape further away.
	- **slope_lines_contours_search_radius** (meters) — how far around a Slope Line's own position Step 1 searches for the nearest Contour Raster pixel to attribute its gravity reading to. A Slope Line's placement on the map is not always pixel-exact on top of its own contour, so a plain under-the-point lookup misses legitimate readings; too small and this still happens, too large and a reading risks being attributed to the wrong, merely-nearby contour instead.
	- **rain_drop_step** (meters) — distance a rain drop advances per simulation step, shared by every variant of the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c). Should stay small relative to **rasterization_px_size** (a few pixels at most); [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc)'s pixel walk still finds every pixel a step crosses even for a large step, but a coarse step blurs exactly where along the path a vote or an evaporation was triggered and raises the chance of overshooting the map boundary or the source's own contour.
	- **sources_per_contour_segment** (pure number) — how many sources are placed per contour segment, shared by every variant of the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c). Should scale with how long segments are relative to **rain_drop_step**: the larger **contours_step** is, the higher **sources_per_contour_segment** needs to be to keep sources close enough together to resolve nearby thin contours individually.
	- **rain_drop_starting_voting_hysteresis** (pure number, counted in **rain_drop_step**-sized steps) — Cold-only: a Cold rain drop is exempt from evaporating on re-crossing its own starting contour within this many steps of being created, and, separately, exempt from evaporating on re-crossing any other contour it has already voted for within this many steps of *that vote* (see the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)). Should be just large enough (a handful of steps) to carry the drop clear of either; since each exemption is measured from its own reference point (creation, or the vote itself), it does not need to be sized relative to the distance to neighboring contours or to how far into the drop's path a vote happens to fall.
	- **undefined_gravity_vote_threshold** (pure number, a ratio in (0, 1)) — Cold-only: how close left and right vote counts must be before being flagged as ambiguous (Step 3). A value near 1 only flags near-exact ties; a value near 0 flags votes on both sides however lopsided.
	- **out_of_bound_extra_dilation** (pure number, a non-negative integer) — how many extra rounds of 8-connected dilation are run, after the out-of-bound flood-fill reaches its own fixed point, growing the out-of-bound (`1`) area inward over *any* pixel value rather than only undefined (`0`) ones (Step 1). Zero stops the out-of-bound area exactly where the flood-fill's own firebreak left it; a larger value pads the map edge further in, at the cost of overwriting genuine contour/density/no-contour-in-bound pixels near the border.
	- **growing_oob_seeking_max_steps** (pure number, a non-negative integer) — how many steps a Flying End spends in the Growing Process's first pass, seeking the out-of-bound area entirely on its own (matching against another Flying End disabled outright), before falling through to the second pass (matching restored, the out-of-bound attraction dropped) for its remaining steps (Step 1's Growing Process). `0` turns the first pass off outright — every Flying End starts directly in the second. Should be a handful of **contours_step**-sized steps: enough to actually reach a border that's genuinely close, not so many that a Flying End with none nearby wastes a long detour before it gets to try matching instead.
	- **growing_window_size_px_contours** (pixels, a non-negative integer) — the Growing Process's own square scan window size for contour-pixel repulsion (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): a real contour pixel or a **TEMPORARY_CONTOUR** tail only repels within a window of `int(growing_window_size_px_contours / 2) * 2 + 1` pixels per side -- except the Flying End's own pixel and its 8 immediate neighbors, always excluded regardless of this setting, since the Flying End always sits right on top of its own just-written body there, and that self-proximity would otherwise swamp the term with a huge, meaningless push instead of reflecting genuinely nearby contour pixels. Set directly rather than derived from **contours_step**/**rasterization_px_size**, so the window's own reach and the algorithm's other distances can be tuned independently. Should be wide enough to comfortably reach a couple of **rasterization_px_size**-sized pixels past **contours_step** away, and more than just `2` (or nothing at all past the excluded 8 neighbors will ever be seen).
	- **growing_window_size_px_attractions** (pixels, a non-negative integer) — the same, for attraction (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): an out-of-bound (`1`) or high-density (`3`) pixel, and another pending Flying End to match against (case (a)), are only seen within a window of `int(growing_window_size_px_attractions / 2) * 2 + 1` pixels per side. Kept separate from **growing_window_size_px_contours** so how far the Growing Process reaches for something to move toward can be tuned independently of how far it reaches for something to repel it.
	- **growing_step_length** (pure number, a fraction of **contours_step**) — how far each of the Growing Process's own case-(c) steps actually moves (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): `growing_step_length * contours_step`. `1.0` moves the full **contours_step**, matching the equal-chord resampling distance every other node uses; a smaller value takes more, shorter steps instead, reacting to nearby pixels more often at the cost of needing more steps to reach the border. Case (b)'s own snap distance scales the same way, so a smaller step length doesn't leave case (b) settling on a pixel case (c) wouldn't itself have reached yet.
	- **growing_previous_distance_direction_weight** (pure number) — how strongly the Growing Process's next step favors continuing in the direction the contour was already heading, relative to the pull of nearby out-of-bound/high-density/other-contour pixels (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)).
	- **growing_out_of_bound_direction_weight** (pure number, positive) — how strongly an out-of-bound (`1`) pixel attracts the Growing Process toward it ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)).
	- **growing_density_direction_weight** (pure number, positive) — how strongly a high-density (`3`) pixel attracts the Growing Process toward it ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)).
	- **growing_other_contours_direction_weight** (pure number, negative) — how strongly any contour pixel repels the Growing Process away from it ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)). Negative, unlike the other two `growing_*_direction_weight` parameters, since a contour is something to grow around rather than toward.
	- **growing_visualization_push_pull_vectors_scale** (pure number) — purely a visualization knob: how much `01_<map_name>_step1_growing.svg` (see Visualization, below) scales the four push/pull vectors it draws for each growing step's own case (c) before drawing them, so their length is legible against the map's own scale. Never affects the Growing Process itself.
- ## Rain Drop Production Definition
  id:: 6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c
  collapsed:: true
	- A **Rain Drop Production** simulates a set of particles ("rain drops") walking away from a set of contours, to sense what lies around them. Every variant used elsewhere in this document — in Step 1, Step 3, and Step 4 — is one of four combinations of two independent choices, **Direction** and **Temperature**. This section defines what all four share; what a specific call site does *at* an evaporation (vote for a gravity side, mark a pixel, assign an elevation) is documented at that call site, not here — exactly as [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc) itself only answers "did this step cross something, and what," leaving what the caller does about it to the caller.
	- **Source placement** (shared by all four variants): given a contour, for each segment of its **LineString** we create **sources_per_contour_segment** **sources**, equally spaced. If **sources_per_contour_segment** is 3 we create a source at A, A + 0.33\*(B-A) and A + 0.66\*(B-A), where A and B are the starting and ending **Coord**s of the segment.
	- **Source direction** (shared by all four variants): each source's own direction (perpendicular to the contour, in accordance with its gravity — real gravity for a contour that already has one, or a call site's own placeholder otherwise, as Step 1 does) is a blend of the direction *at* node A and the direction *at* node B, weighted by the same fraction used to place that source along the segment -- the source at A is 100% A's own direction, the one at A + 0.33\*(B-A) is 66% A's direction and 33% B's, and so on, rather than every source along a segment leaving in the same, flat direction. A node's own direction is the mean of the perpendicular of the segment before it and the one after (a node with only one neighboring segment -- an open contour's first or last node -- takes that one alone; a closed contour's first node wraps its "previous" segment around to the contour's own last one). The direction of each rain drop never changes once it starts.
	- **Direction — Rain vs Anti Rain**: a **Rain Drop** moves in its source's own direction; an **Anti Rain Drop** moves in exactly the opposite direction. Nothing else differs between the two.
	- **Stepping and hit detection** (shared by all four variants): we let each rain drop step **rain_drop_step** meters at a time. To check what a step crosses, see [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc), which reports the first of: a specific contour (by index), an out-of-bound (`1`) pixel, or a high-density (`3`) pixel that the step touches, or reports the step as clear if none of these occurs. A rain drop's own source contour never counts as a contour hit against itself, for its whole lifetime (Appendix 4) — otherwise it would evaporate against its own origin on its very first step.
	- **Temperature — Cold vs Hot** (governs only when a drop evaporates; independent of Direction):
		- A **Cold** rain drop evaporates when it crosses an out-of-bound (`1`) pixel, or when it crosses a contour whose gravity is already defined — except within **rain_drop_starting_voting_hysteresis** steps of its own creation if that contour is its own starting one, or within **rain_drop_starting_voting_hysteresis** steps of having voted for a given contour if it's crossing that same contour again; past either window, crossing it does evaporate the drop. A Cold rain drop passes through a high-density (`3`) pixel untouched — density is not a Cold trigger.
		- When a Cold rain drop crosses a contour with *undefined* gravity, it doesn't evaporate: it casts a vote for the corresponding gravity side on that contour and keeps travelling (to avoid voting twice for the same contour, a drop remembers every contour index it has already voted for, together with the step it voted on — this is what the hysteresis exemption above measures from).
		- A **Hot** rain drop evaporates when it crosses *any* contour at all — gravity defined or not — or when it crosses a high-density (`3`) pixel, or when it crosses an out-of-bound (`1`) pixel. **rain_drop_starting_voting_hysteresis** and **undefined_gravity_vote_threshold** are Cold-only: a Hot rain drop never votes, so neither applies to it.
	- This gives four named variants used across the rest of this document: **Cold Rain Drop Production**, **Cold Anti Rain Drop Production**, **Hot Rain Drop Production**, and **Hot Anti Rain Drop Production**.
- ## Step 1: Extrapolate Elevation Information from an .omap
	- Given an .omap file we extrapolate the following symbols divided into three different families:
		- 1) **Contours**: *[Index] Contour*
		- 2) **Slope Lines**: *Slope Line (only the ones for contours, not the ones for form lines)*
		- 2) **Jumps**: *Earth Bank [minimum size], [Small] [Impassable] Cliff [minimum size][Small]*
		- 3) **Heavy Objects**: *Erosion Gully, [Small] [Crossable] Watercourse, Water Channel*
	- We instantiate a **Vector** of **Contours** for each contour in the .omap file. An idea of the struct definitions is shown:
	  ```rust
	  struct GravityVotes{
	      /// With left or right gravity we mean: standing on the first point
	      /// of the LineString and looking toward the second point of it.
	      left: u64,
	      right: u64
	  }
	  
	  struct LineWithGravity{
	      /// It's the definition of a line that is always perpendicular to gravity,
	      /// the LineString defines the nodes of the line and its
	      /// gravity direction (one of the two possible ones) is defined by the
	      /// (ls[0].x + gravity_dx, ls[0].y + gravity_dy) vector.
	      /// That vector must be perpendicular to the segment ls[0] -> ls[1].
	      /// It must hold that (gravity_dx*gravity_dx + gravity_dy*gravity_dy) == 1.
	      ls: LineString<f64>,
	      gravity_dx: Option<f64>,
	      gravity_dy: Option<f64>,
	      gravity_votes: GravityVotes,
	  }
	  
	  struct Contour{
	      lwg: LineWithGravity,
	      elevation_height: Option<f64>,
	  }
	  ```
	  Where the .omap node sequence is transformed into a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** by the steps described in [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8).
	- **Gravity direction semantics**: `gravity_dx`/`gravity_dy` are only ever *fixed* relative to the first segment (`ls[0] -> ls[1]`) — they are not meant to be compared directly, as a raw vector, against a gravity reading taken somewhere else along a curved contour. What they really encode is a **side**: since a contour's gravity direction is the same everywhere along it (see Assumption 1), fixing the perpendicular direction at `ls[0]` is equivalent to picking one of the two sides of the LineString (left or right of its parameterization direction) as "downhill" for the whole contour. To get the actual gravity vector at any *other* point of the contour — e.g. to compare against a `PointGravityDefiners` reading in Step 2, or to know which way a rain drop should leave a source in Step 3 — compute the local tangent at that point from its neighboring **Coord**s, take its two perpendicular unit vectors, and pick the one on the same side — left/right, via the sign of the 2D cross product against the tangent — that `(gravity_dx, gravity_dy)` is on relative to the `ls[0] -> ls[1]` tangent:
	  ```rust
	  use geo::Coord;
	  
	  /// Which side of the tangent `tangent_from -> tangent_to` the vector
	  /// `(dx, dy)` points to: a positive result means left, a negative
	  /// result means right. Comparing the sign computed here at
	  /// `ls[0] -> ls[1]` against the sign computed at any other point's
	  /// local tangent is how "in accordance" is decided everywhere in
	  /// Step 2 and Step 3 — never a direct comparison of raw (dx, dy)
	  /// components, which are only valid relative to the tangent they
	  /// were computed against.
	  fn side_of_tangent(tangent_from: Coord<f64>, tangent_to: Coord<f64>, dx: f64, dy: f64) -> f64 {
	      let (tx, ty) = (tangent_to.x - tangent_from.x, tangent_to.y - tangent_from.y);
	      tx * dy - ty * dx
	  }
	  ```
	- We instantiate a 2D **Vector** of **u32**, the **Contour Raster**, with a pixel size in meters defined by the **rasterization_px_size** parameter. Each pixel holds one of five reserved values, or a shifted contour index:
		- `0` — undefined: nothing has claimed this pixel yet.
		- `1` — out of bound.
		- `2` — no contour, but in bound.
		- `3` — high density: two or more contours conflicted here, or a **Jump**'s own area (see below).
			- **TEMPORARY_CONTOUR** (currently `4`) — a Flying End's own not-yet-final tail, one grow step at a time, during the Growing Process (see below): a repeller for every other Flying End's own window scan, before it resolves under its real contour index. Never seen in any output -- every remaining one is swept back to `2` once the whole Growing Process finishes, well before Step 2 ever runs.
		- **CONTOUR_0_MATRIX_VALUE** + i — contour i (the index inside the previously defined **Vector** of **Contours**). **CONTOUR_0_MATRIX_VALUE** is an internal constant of the program, not a config-file parameter — currently `5` — kept as a single named constant precisely so a future shift in these reserved values only has to change one place.
	- The Contour Raster is built in four steps:
		- 1) Every pixel starts at `0` (undefined).
		- 2) For each contour, for each consecutive pair of **Coord**s of its **ls**, we walk every pixel that the segment between them touches using the pixel walk described in [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc) (the source-contour exclusion from that appendix does not apply here — every touched pixel is written the same way), writing **CONTOUR_0_MATRIX_VALUE** plus the contour's index. If a touched pixel already holds that same value, it's a no-op. If it's `1` (out of bound), it's left alone — that's the map's own edge, not another contour, and a contour's own last segment can legitimately run right up to (and through) one on purpose (see the Growing Process below); overwriting it, with either the contour's own value or `3`, would misreport genuine out-of-bound territory as something it isn't. If it's **TEMPORARY_CONTOUR**, it's free to claim too, exactly like `0` or `2` -- that's precisely how a Flying End's own tail, marked temporary one grow step at a time, becomes real once that contour finally resolves (see the Growing Process). If it already holds any other *different* value — another contour's, or already `3` — we set it to `3` (high density) instead and keep going: real contour digitizing sometimes places two physical lines closer together than **rasterization_px_size** can tell apart (e.g. at cliffs), and marking the pixel high density rather than crashing is how the rest of the algorithm is told not to trust it as belonging to any one contour.
		- 3) For each contour, we run one **Hot Rain Drop Production** and one **Hot Anti Rain Drop Production** (see [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)) — since no contour has a real gravity direction yet at this point, either perpendicular side is used as a throwaway placeholder "gravity" for this purpose only: a Rain Drop Production and its Anti Rain counterpart together always cover both perpendicular sides of a contour regardless of which side got the placeholder label, so the choice can't affect the result. Each drop accumulates, across every step of its own path, every pixel that step's own Appendix 4 walk touches while still `0` — but only actually sets them to `2` once the drop itself evaporates, and only if it evaporates by hitting a contour or a high-density pixel, *not* by leaving the map. A drop that leaves the map must not have any of its path committed: doing so would plant a stretch of `2` pixels between the map's real out-of-bound area and its own border, which step 4 below can only ever flood-fill through `0` pixels — those pixels would be stuck showing "in bound" when they are really outside the map.
		- 4) We compute out of bound:
			- Every pixel on the raster's own border (its first and last row, first and last column) is set to `1`.
			- We then flood outward 8-connectedly from there: an **active_out_of_bound_pixels** list starts as every pixel currently `1`. Each iteration, for every pixel in that list, we check its 8 neighbours; any neighbour currently `0` is set to `1` and collected into a fresh **active_out_of_bound_pixels** list for the next iteration. We stop once an iteration collects nothing. Since this only ever turns a `0` into a `1`, it can never eat into a contour, high-density, or no-contour-in-bound pixel -- those act as a firebreak, which is exactly how a `0` pixel fully enclosed by them can legitimately stay `0` (undefined) forever.
			- We then run **out_of_bound_extra_dilation** further rounds of the same 8-connected dilation, but this time *any* value (not only `0`) one step away from a `1` pixel becomes `1` too -- this is the one part of the procedure that can overwrite a genuine contour/high-density/no-contour-in-bound pixel, deliberately padding the out-of-bound area further into the map near its edge.
	- All the other symbols (**Slope Lines**, **Jumps** and **Heavy Objects**) are just used to understand the gravity direction and are stored inside a **Vector** of instances of **LineGravityDefiners** and a **Vector** of **PointGravityDefiners** in the following way:
		- The **Slope Lines** are point symbols whose gravity direction can be read directly from the .omap file. For each one, we search the Contour Raster for the nearest pixel with a value of **CONTOUR_0_MATRIX_VALUE** or above, within **slope_lines_contours_search_radius** ground meters of its position (a Slope Line's placement on the map is not always pixel-exact on top of its own contour): if none is found, we emit a warning and skip it; otherwise we record that pixel's reference contour (the pixel's value minus **CONTOUR_0_MATRIX_VALUE**) and append it to the **Vector** of **PointGravityDefiners.**
		  ```rust
		  struct PointGravityDefiners{
		  	x: f64,
		      y: f64,
		      reference_contour: u64,
		      gravity_dx: Option<f64>,
		      gravity_dy: Option<f64>,
		  }
		  ```
		- The **Jumps** are line symbols that should first of all be transformed into a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** following [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8). After that they should be transformed into polygons following [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e). Before that polygon's own area is touched, we scan it the same way a Heavy Object's own polygon is scanned below, grouping its matched pixels by contour index and recording each distinct contour's own matched-pixels' centroid, into **touched_contours** — since every pixel of the Contour Raster whose center falls inside the polygon is *then* set to `3` (high density), independently of whatever value it held before (a Jump is real terrain, and the rest of the algorithm should treat the ground under and around it the same way it treats a too-closely-bunched cluster of contours), which would otherwise erase the very evidence, a plain contour value, of which contours were under it. After that we append them to the **Vector** of **LineGravityDefiners**, a struct defined below, where the **ls** of `LineWithGravity` should be the **LineString** resulting from Appendix 1 and the **gravity_direction** should be retrieved from the .omap object.
		  ```rust
		  struct LineGravityDefiners{
		      lwg: LineWithGravity,
		      poly: Polygon<f64>,
		      /// Every contour the polygon covered, and that contour's own
		      /// matched-pixels' centroid, captured before the polygon's own
		      /// area was set to high density.
		      touched_contours: Vec<(u64, Coord<f64>)>,
		  }
		  ```
		- The **Heavy Objects** are line symbols, so they should also first of all be transformed into a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** following [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8), then into a buffered polygon following [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e) (using the same **heavy_object_width**/**heavy_object_growing** parameters a Jump's own polygon uses). The difference with respect to **Jumps** is that their gravity direction cannot be obtained from the .omap file directly, but needs to be inferred by considering the shape of the contours that intersect with them and the fact that they are not lines always perpendicular to gravity, but always parallel to it. We can still get information from them in the following way. We rasterize the buffered polygon (the same way a Jump's own polygon is, in Step 2) and, for each of its pixels with a value of **CONTOUR_0_MATRIX_VALUE** or above in the 2D Contour Vector, we have a candidate intersection with that contour (remember to subtract **CONTOUR_0_MATRIX_VALUE** from the value to get the index). Searching the whole buffered area, not just the pixels directly under the digitized line, catches a contour that runs close to a Heavy Object without landing exactly on it — the same motivation as **slope_lines_contours_search_radius** for Slope Lines. Since the polygon commonly covers several pixels of the very same nearby contour, we group its matched pixels by contour index and treat each distinct contour as a single intersection, at the centroid of that contour's own matched pixels within the polygon, rather than one intersection per pixel. For each such intersection we should compute the circumference that fits the contour in the section around it (the number of **Coord**s to use before and after the intersection for the fit is defined by a parameter, **circumference_fitting_points_number**), and we should define the gravity of that intersection based on the vector starting from the intersection and ending at the center of the fitted circumference. Now we have a point, we know the reference contour, and we also know the gravity direction, so we can append an item to the **Vector** of **PointGravityDefiners**.
	- **Closing dangling ends: the Growing Process.** After every other Step 1 sub-step above has run — including Slope Lines, Jumps, and Heavy Objects, so the Contour Raster's out-of-bound and high-density values are final -- every open contour's start and end node should resolve to an out-of-bound (`1`) or high-density (`3`) pixel (closed contours have no ends, so they're exempt).
		- We first build the list of **Flying Ends**: every open contour's start or end node whose own Contour Raster pixel is neither `1` nor `3`.
		- Every Flying End then runs its own **Growing Process** in two passes, **Seeking** then **Matching**:
			- **Seeking**: up to **growing_oob_seeking_max_steps** steps, entirely on its own -- case (a) below (matching against another Flying End) is disabled outright, and case (c)'s direction never includes the out-of-bound attraction term. A Flying End that resolves (case (b) or (c)) within this budget is done; one that doesn't falls through to **Matching** for its remaining steps. This exists because two contours that happen to run close and parallel near the border can land in each other's window purely because they're both, independently, heading for that same border -- not because they're actually the same physical line split in two -- and without a matching-free pass first, case (a) (checked before case (b) or (c) ever get a look) would snap them together instead of letting each reach the border on its own, even when each already has a perfectly good border of its own close by.
			- **Matching**: the full process -- case (a) restored, but case (c)'s direction now drops the out-of-bound attraction term instead (having already failed to find a border on its own during Seeking, dropping the pull toward one lets it settle into matching a nearby Flying End instead of still chasing a border it hasn't found).
			- Both passes are round-robin rather than one Flying End at a time to completion: each still-unresolved Flying End takes exactly one growing step below, then the whole list is cycled through again, and so on until none are left in that pass (resolving one, in **Matching**, can also resolve another already in the list -- case (a) below). Each step of a Flying End's Growing Process, in either pass:
			- A new node is never drawn into the Contour Raster under its own contour's real index the moment it's added to a contour's `ls` -- only once that `ls` reaches the truly final shape it will keep, in case (a) or (b) below. A node placed by case (c) is, until then, recorded only in the contour's own `ls`; a later resample (closing or merging, in case (a), or snapping, in case (b)) can still shift nodes placed long before, not only the newest one, out of phase with whatever pixels their earlier position had already claimed. Whenever that happens, the contour's (both contours', for a merge) entire previous Contour Raster footprint is cleared first, then the new, final `ls` is drawn into it fresh, rather than drawn additively on top of the old.
			- That said, a step placed by case (c) is not entirely invisible to the raster in the meantime: the segment it just walked is immediately marked **TEMPORARY_CONTOUR**, so a *different* Flying End's own window scan, running its own turn a moment later, still repels off of it -- without this, two Flying Ends growing at the same time, each still with nothing drawn under a real index, could fly straight through each other. Every remaining **TEMPORARY_CONTOUR** pixel is swept back to `2` (no contour, in bound) once every Flying End in both passes has resolved, not per contour as each one resolves -- a still-flying neighbor may still be relying on that same pixel as a repeller.
			- Looks at two square windows centered on the Flying End's own pixel: one of `int(growing_window_size_px_attractions / 2) * 2 + 1` pixels per side for another Flying End to match against (case (a)) or an out-of-bound/high-density pixel to be pulled toward (case (b)/(c)), and one of `int(growing_window_size_px_contours / 2) * 2 + 1` pixels per side for a contour pixel to be pushed away from (case (c)) -- kept separate so how far the process reaches for something to move toward can be tuned independently of how far it reaches for something to repel it. The contour window always excludes the Flying End's own pixel and its 8 immediate neighbors, regardless of **growing_window_size_px_contours**: that 3-by-3 block is always the Flying End's own just-written body, and reacting to it there would be reacting to itself rather than to a genuinely nearby contour.
			- (a) **Matching only** (see above). If another Flying End's pixel is inside the attraction window, the new node is placed exactly on top of it. If that other Flying End belongs to a *different* contour, the two contours are merged end to end (oriented so the two close ends meet), with the merged `ls` re-run through [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)'s equal-chord resampling, and one of the two contour indices is discarded at random, every higher index shifted down by one everywhere it's used -- the Contours vector, every Contour Raster pixel encoding a higher index, and every already-collected `PointGravityDefiners.reference_contour`. Either merged contour's *other* end, if it is itself still a pending Flying End waiting on its own later turn, is also remapped rather than simply shifted: it keeps following its own contour (now the kept index) and becomes that contour's new start or end -- whichever end this merge didn't just consume always ends up at the front of the merged `ls` for the kept contour and at the back for the discarded one, regardless of which of the two contours' own start/end nodes happened to be the ones that just met. If it instead belongs to the *same* contour -- its own other end, a valid and good match here, not a case to skip -- that contour is closed into a ring instead: the same new node closes it (its first and last `Coord` become the same point), and it is re-run through Appendix 1's equal-chord resampling the same way any closed contour is, shrinking the step to evenly divide the perimeter rather than leaving a short closing segment. Either way, the Growing Process is finished for both Flying Ends involved, and the resulting `ls` (or `ls`es) is (re)drawn into the Contour Raster as described above.
			- (b) Else, if the window holds a `1` or `3` pixel closer than **growing_step_length * contours_step** (the same distance case (c) would otherwise move by, so a pixel case (c) would already reach or pass is settled on directly here instead; the nearest one, if several qualify), the new node is placed at that pixel's own center, and the `ls` is re-run through Appendix 1's equal-chord resampling (its final segment is now shorter than **contours_step**). The Growing Process is finished, and the resampled `ls` is (re)drawn into the Contour Raster as described above. Unaffected by which pass is running -- reaching a border or a density pixel close enough to settle on is a success either way, not something Seeking should hold back on.
			- (c) Else, the new node is placed **growing_step_length * contours_step** away from the current Flying End, in the direction [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e) computes from every `1`, `3`, **TEMPORARY_CONTOUR**, or contour pixel (any contour, **CONTOUR_0_MATRIX_VALUE** or above) found in the window, blended with the direction the contour was already heading in -- except that during **Matching**, `1` pixels are left out of this weighted sum entirely (see above); **TEMPORARY_CONTOUR** counts as a contour pixel here regardless of pass, repelling exactly the same way. If the new node's own pixel is `1` or `3`, the Growing Process is finished, and the `ls` (no resampling needed here -- this step's own node is already exactly **growing_step_length * contours_step** from the last one) is drawn into the Contour Raster the same way as any other contour node (step 2 of the Contour Raster's own construction above); the node's own pixel, already `1` or `3`, stays exactly that (an out-of-bound pixel this ending touches is left as `1`, per step 2's own exception, not turned into a conflict); otherwise the new node is the contour's new Flying End -- the segment just walked is marked **TEMPORARY_CONTOUR** as described above, but nothing is drawn under a real contour index yet -- and the process repeats from its own window (in whichever pass it's still in).
- ## Step 2: Obvious Gravity Definition
  collapsed:: true
	- We scan all the items in the **Vector** of **PointGravityDefiners** and, for each one, we set the gravity direction of its reference contour if not already defined, or check whether it is in accordance with the already-set gravity otherwise. The algorithm should crash with an error explaining the problem if they do not match.
	- We scan all the items in the **Vector** of instances of **LineGravityDefiners** and, for each of its own **touched_contours** entries (not by re-rasterizing **poly** against the Contour Raster here -- by this point its own area is high density, not the contours it used to show, so nothing would be found), we set the gravity of that contour to the gravity of the **LineGravityDefiners** if it is not already defined, or check whether it is in accordance with the already-set contour gravity otherwise. The algorithm should crash with an error explaining the problem if they do not match.
	- We scan all the closed contours (we should check whether the LineString is closed) without a defined gravity direction. If they do not contain any other contours (see [Appendix 3](6aa14c07-a160-43d7-845b-66e9462a8bd2)), we assign the gravity direction to be opposite to the enclosed area (we assume that closed contours without slope lines are hills).
	- If all the contours have the gravity direction defined, we will skip Step 3.
- ## Step 3: Gravity Definition with Rain
	- We iterate over all the contours that already have a gravity defined, and for each of them we start a **Cold Rain Drop Production** (see [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)).
	- When all the rain drops from this pass have evaporated, we finish the pass by defining the gravity of each contour based on the votes it received. Contours with similar left and right votes should be reported as **warnings** from the algorithm. We define left and right votes as similar if the smaller of the two votes is more than **undefined_gravity_vote_threshold** of the other.
	- If all the contours have the gravity direction defined, the step is finished.
	- We iterate over all the contours that already have a gravity defined (now possibly more of them than before this pass), and for each of them we start a **Cold Anti Rain Drop Production** — identical to a Cold Rain Drop Production, but every drop moves against gravity instead of along it.
	- At this point it should be impossible to have contours with undefined gravity. Actually is possible with a non small enough **sources_per_contour_segment**
- ## Step 4: Elevation Value Assignation
	- Assuming that each contour has the gravity direction defined (the algorithm should crash otherwise)
	- Targeting a contour called **C**, we run a **Hot Rain Drop Production** from it (see [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)): since every contour already has its gravity defined by this point, a Hot rain drop's evaporation always happens on some contour (or on high density, or out of bound, which set nothing). When it evaporates by hitting another contour, we set that contour's **elevation_height** based on the **accordance** or **discordance** of the rain drop's own direction and that contour's gravity direction. In case of **accordance** we set the height as the **C** height minus 1. In case of **discordance** we set it as the same height value of **C**. If the hit contour already has a height value, check that it's equal to the one we would have written, otherwise crash with an error (this should not be possible to happen).
	- In a similar way we can define also a **Hot Anti Rain Drop Production** exactly equal to Hot Rain Drop Production but with an anti-rain drop instead.
	- A random contour is selected and its **elevation_height** is set to 0.
	- From the random selected contour we execute a Hot Rain Drop Production and a Hot Anti Rain Drop Production keeping tracks (in both) of all the contours that we are setting the height off in a list that we will call **LHS**= List of Height Set (of course we do not add at the list the contours that has been hitted but already had a defined height).
	- When all the rain drops evaporates: for each element inside **LHS** we compute again a Hot Rain Drop Production + Hot Anti Rain Drop Production phase and we continue recursively.
	- At this point, considering a small enough **sources_per_contour_segment** all contours should have a defined height. With a generic parameter value could be that some contours has still not a defined height. Anyway instead of crashing we can still try to recover the contours that still are missing an height (list of contours that we call **AC**=Alone Contours) as described in the following.
	- For each contour in **AC** we run a **Reverse Hot Rain Drop Production** + **Reverse Hot Anti Rain Drop Production** that are working exactly like their non *reverse* version but instead of setting the hitted contour height based on the source contour they set the height of the source based on the hitted contour height value.
	- If there are still contours without set height the algorithm should crash suggesting a smaller **sources_per_contour_segment**.
- ## Step 5: Final Tiff Computation
	- In this phase, given the height of each contour we have to write a new raster 2D vector of pixel size of **elevation_raster_pixel_size** in meters.
	-
- ## Visualization
	- For a human check of each step, I would like to see each step, one after the other, as an SVG file. The files are numbered in the order they're produced: `00_<map_name>_step1.svg`, `01_<map_name>_step1_growing.svg`, `02_<map_name>_step2.svg`, `03_<map_name>_step3_rain.svg`, `04_<map_name>_step3_anti_rain.svg`.
	- Every SVG below that draws the Contour Raster grid draws, under everything else, a single black polygon for the whole out-of-bound (`1`) area, some gray lines showing the pixel grid, plus a square for every pixel that is a contour (brown, **CONTOUR_0_MATRIX_VALUE** or above, the same brown regardless of which contour) or high density (light green) — `0` (undefined) and `2` (no contour, in bound) get no square of their own, since together with `1` they typically cover most of a real map's own raster and squaring every one of them made these files huge. Out of bound gets a shape rather than nothing, since seeing where it actually reaches matters for judging **growing_oob_seeking_max_steps**/**out_of_bound_extra_dilation**, but not a square per pixel either, for the same file-size reason: traced instead into a handful of closed rings (the raster's own bounding rectangle, plus one ring per out-of-bound/in-bound transition found) and drawn as one path with an even-odd fill rule, so the true shape (holes and all) comes through in space proportional to its own perimeter, not its area. The full, every-pixel-colored picture (light grey for `0`, black for `1`, white for `2`, light green for `3`, brown for a contour) instead goes into a companion `<map_name>_raster.png`, written once alongside `01_<map_name>_step1_growing.svg` (the Contour Raster never changes again past that point).
	- `00_<map_name>_step1.svg`, written once Step 1's Contour Raster (including the Jump high-density stamping and the out-of-bound computation) is complete, but before Step 1's own Growing Process sub-step runs: draws, on top of the palette above, two lines for each contour: one including the Bezier segments, in red, and one after linearization with only straight-line segments, in green. It should also print a blue arrow for each **LineGravityDefiners**, in the direction of the gravity it defines, starting from its definition location, and print several yellow arrows for each **LineGravityDefiners**, spanning the whole polygon it occupies. Draw each **LineGravityDefiners**' own buffered polygon filled, lightly, so the actual search area a **heavy_object_width**/**heavy_object_growing** choice produces can be judged by eye against the real pixels; draw every Heavy Object's own buffered polygon the same way, in a different fill color so the two are easy to tell apart. Additionally, draw a red ring (stroke only, no fill) centered on every Flying End's position — every open contour's start/end node not yet resolved to an out-of-bound or high-density pixel, before the Growing Process runs.
	- `01_<map_name>_step1_growing.svg`: identical to `00_<map_name>_step1.svg` — including the same red rings, still shown at their original, pre-growing positions, kept for reference — except that it's written after the Growing Process has run, so the Contour Raster and every contour's `ls` reflect the post-growing state, and the post-linearization line (the green one above) of any contour touched by growing — grown, merged into another, or both — is drawn in blue instead. The red, pre-linearization layer is unaffected by a merge: it is always one subpath per originally-digitized contour object, drawn from that object's own raw node sequence alone, never spliced across a merge into a second, geometrically unrelated object's own raw trace (there is no meaningful single curve through two originally separate digitized objects) — so `01_<map_name>_step1_growing.svg` can show fewer distinct contours in blue/green than it does in red, once any contours have merged. Additionally, for every growing step that went through case (c) ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)), draw the four push/pull contributions computed there as four separate vectors, tail on the Flying End's own position *before* that step, head at tail plus that contribution scaled by **growing_visualization_push_pull_vectors_scale** — so a vector's own length shows how strongly it pulled or pushed, not just its direction. One color per contribution: deep sky blue for the **growing_previous_distance_direction_weight** term (continuing the contour's own heading), magenta for the **growing_out_of_bound_direction_weight** term (zero-length wherever it was dropped entirely, during **Matching**), dark green for the **growing_density_direction_weight** term, and maroon for the **growing_other_contours_direction_weight** term.
	- `02_<map_name>_step2.svg`: like `01_<map_name>_step1_growing.svg`, and in addition, for each contour with a defined gravity, write yellow arrows all along the contour showing the gravity direction.
	- `03_<map_name>_step3_rain.svg` and `04_<map_name>_step3_anti_rain.svg`: each doing the same as `02_<map_name>_step2.svg`, one also showing the rain drops' paths as blue dots, the other also showing the anti rain drops' paths as red dots. Anti Rain Drop Production always runs right after Rain Drop Production and both write gravity into the same contours, so by the time either file is written every contour either pass ever resolved already has its final gravity -- the rain file's own gravity-arrow layer must still be limited to whichever contours were already resolved by the time Rain Drop Production itself finished (Step 2's own resolutions, plus Rain Drop Production's), not extended to ones only Anti Rain Drop Production goes on to resolve afterward, or a contour no rain drop ever reached would wrongly show a gravity arrow there anyway. The anti rain file, written once both passes are done, shows the full final state. Under every drop's own dots, first trace its whole trail (source to evaporation) as a thin gray line, a third of the width used below for a vote segment, so the path itself reads as a line rather than a scatter of unconnected dots. Over that, under any dot still within that drop's **rain_drop_starting_voting_hysteresis** window, draw a black dot with a slightly larger radius, so it still peeks out from underneath. A vote belongs to the step it happened on, not to either endpoint alone, so draw it as a purple line segment from the drop's previous position to its current position at the moment it cast the vote, rather than a dot at a single point.
- ## Appendix 1
  id:: 6aa103ad-0d81-46fc-bc7e-5d07c29b4be8
  collapsed:: true
	- The nodes of a contour will in general be a sequence of segments that can be of two types: a straight line or a cubic Bezier curve. Before starting the algorithm, each contour needs to be converted into an instance of the **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** struct of the [geo](https://crates.io/crates/geo) crate, where the distances between consecutive **[Coord](https://docs.rs/geo/0.33.1/geo/geometry/struct.Coord.html)**s should always be equal to **contours_step** meters. It should be noted that closed lines in .omap should also be closed lines as a **LineString**; they should result in `closed_line.is_closed() == true`.
	- To do so, first of all, we should create a **LineString** without the constraint that consecutive **Coord**s be equally spaced. For segments of the contour that are already a straight line we do not need to do anything. For segments that are a cubic Bezier curve we should apply the following algorithm, which *linearizes* a cubic Bezier curve into straight line segments of length smaller than **step**. We call this parameter, in meters, **bezier_linearization_step**.
	  ```rust
	  use geo::{Coord, LineString};
	  
	  fn lerp(a: Coord<f64>, b: Coord<f64>, t: f64) -> Coord<f64> {
	      Coord { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }
	  }
	  
	  /// Split a cubic Bezier at t = 0.5 into two cubic Beziers (De Casteljau).
	  fn subdivide(
	      p0: Coord<f64>, p1: Coord<f64>, p2: Coord<f64>, p3: Coord<f64>,
	  ) -> ([Coord<f64>; 4], [Coord<f64>; 4]) {
	      let p01 = lerp(p0, p1, 0.5);
	      let p12 = lerp(p1, p2, 0.5);
	      let p23 = lerp(p2, p3, 0.5);
	      let p012 = lerp(p01, p12, 0.5);
	      let p123 = lerp(p12, p23, 0.5);
	      let mid = lerp(p012, p123, 0.5);
	      ([p0, p01, p012, mid], [mid, p123, p23, p3])
	  }
	  
	  fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
	      ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
	  }
	  
	  /// Perpendicular distance from `p` to the infinite line through `a` and `b`.
	  /// Falls back to the distance to `a` when `a == b` (a degenerate chord).
	  fn point_line_distance(p: Coord<f64>, a: Coord<f64>, b: Coord<f64>) -> f64 {
	      let (dx, dy) = (b.x - a.x, b.y - a.y);
	      let len = (dx * dx + dy * dy).sqrt();
	      if len == 0.0 {
	          return dist(p, a);
	      }
	      ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
	  }
	  
	  /// How far the curve's control points bow away from the p0-p3 chord.
	  fn flatness(p0: Coord<f64>, p1: Coord<f64>, p2: Coord<f64>, p3: Coord<f64>) -> f64 {
	      point_line_distance(p1, p0, p3).max(point_line_distance(p2, p0, p3))
	  }
	  
	  fn subdivide_recursive(
	      p0: Coord<f64>, p1: Coord<f64>, p2: Coord<f64>, p3: Coord<f64>,
	      step: f64, out: &mut Vec<Coord<f64>>, depth: u32,
	  ) {
	      // A short chord alone does not mean the curve is flat: a tight loop or
	      // hairpin can have p0 close to p3 while still bowing far away from the
	      // p0-p3 line. Require the curve to be flat *and* the chord to be short
	      // before treating it as a single straight segment.
	      if depth >= 24 || (flatness(p0, p1, p2, p3) <= step && dist(p0, p3) <= step) {
	          out.push(p3);
	          return;
	      }
	      let (left, right) = subdivide(p0, p1, p2, p3);
	      subdivide_recursive(left[0], left[1], left[2], left[3], step, out, depth + 1);
	      subdivide_recursive(right[0], right[1], right[2], right[3], step, out, depth + 1);
	  }
	  
	  fn bezier_to_linestring(
	      p0: Coord<f64>, p1: Coord<f64>, p2: Coord<f64>, p3: Coord<f64>,
	      step: f64,
	  ) -> LineString<f64> {
	      let mut out = vec![p0];
	      subdivide_recursive(p0, p1, p2, p3, step, &mut out, 0);
	      LineString::new(out)
	  }
	  ```
	- When we have a **LineString** we can apply the following algorithm to make its **Coord**s equally spaced (**step** in what follows is what we previously called **contours_step**):
	  ```rust
	  use geo::{Coord, LineString};
	  
	  pub fn resample_equal_chords(ls: &LineString<f64>, step: f64) -> LineString<f64> {
	      assert!(step > 0.0);
	      let pts = &ls.0;
	      if pts.len() < 2 {
	          return ls.clone();
	      }
	  
	      let mut out = vec![pts[0]];
	      let mut cur = pts[0];   // circle centre = last emitted point
	      let mut seg = 0usize;   // segment pts[seg] -> pts[seg+1]
	      let mut t = 0.0f64;     // parameter of `cur` inside that segment
	  
	      'outer: loop {
	          let mut i = seg;
	          let mut t0 = t;
	          loop {
	              if i + 1 >= pts.len() {
	                  break 'outer; // tail shorter than `step`
	              }
	              let (a, b) = (pts[i], pts[i + 1]);
	              if let Some(th) = circle_exit(a, b, cur, step, t0) {
	                  let p = Coord {
	                      x: a.x + th * (b.x - a.x),
	                      y: a.y + th * (b.y - a.y),
	                  };
	                  out.push(p);
	                  cur = p;
	                  seg = i;
	                  t = th;
	                  break;
	              }
	              i += 1;
	              t0 = 0.0;
	          }
	      }
	  
	      // The loop above stops once the remaining tail is shorter than `step`,
	      // without ever emitting the original last point. Always add it back:
	      // for an open input this recovers the (possibly short) final segment,
	      // and for a closed input (`pts[0] == pts.last()`) it is what makes
	      // `closed_line.is_closed() == true` hold on the resampled LineString too.
	      let last = *pts.last().unwrap();
	      if *out.last().unwrap() != last {
	          out.push(last);
	      }
	  
	      LineString::new(out)
	  }
	  
	  /// Smallest t in [t_min, 1] with |A + t(B-A) - C| == r.
	  fn circle_exit(a: Coord<f64>, b: Coord<f64>, c: Coord<f64>, r: f64, t_min: f64) -> Option<f64> {
	      let (dx, dy) = (b.x - a.x, b.y - a.y);
	      let (fx, fy) = (a.x - c.x, a.y - c.y);
	  
	      let qa = dx * dx + dy * dy;
	      if qa == 0.0 {
	          return None; // duplicate points
	      }
	      let qb = 2.0 * (fx * dx + fy * dy);
	      let qc = fx * fx + fy * fy - r * r;
	  
	      let disc = qb * qb - 4.0 * qa * qc;
	      if disc < 0.0 {
	          return None;
	      }
	      let sq = disc.sqrt();
	      for t in [(-qb - sq) / (2.0 * qa), (-qb + sq) / (2.0 * qa)] {
	          if t >= t_min && t <= 1.0 {
	              return Some(t);
	          }
	      }
	      None
	  }
	  ```
- ## Appendix 2
  id:: 6aa11a72-e5ae-4ca3-ab52-379efa00d61e
  collapsed:: true
	- Given a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** we can create a polygon from it and buffer it using the following functions. We refer to **heavy_object_width** and **heavy_object_growing** — both parameters in meters — as `width` and `extra_growing` in what follows.
	  ```rust
	  use geo::{LineString, MultiPolygon, Polygon};
	  use geo::algorithm::{Area, Buffer};
	  
	  fn ls_to_polygon(ls: &LineString<f64>, width: f64, extra_growing: f64) -> Polygon<f64> {
	      let line_with_width: MultiPolygon<f64> = ls.buffer(width);
	      let fat_polygons: MultiPolygon<f64> = line_with_width.buffer(extra_growing);
	      // In theory we should never have more than one polygon because we do not have line
	      // intersection and we use positive buffer sizes, but buffering is numeric and can still
	      // produce extra slivers in degenerate cases (e.g. very tight hairpins). If that happens
	      // we keep the largest polygon and emit a warning rather than silently dropping data.
	      if fat_polygons.0.len() > 1 {
	          eprintln!(
	              "warning: buffering a LineString produced {} disjoint polygons instead of 1; keeping only the largest by area",
	              fat_polygons.0.len()
	          );
	      }
	      fat_polygons.0.into_iter()
	          .max_by(|a, b| a.unsigned_area().total_cmp(&b.unsigned_area()))
	          .expect("buffering a non-empty LineString always yields at least one polygon")
	  }
	  ```
- ## Appendix 3
  id:: 6aa14c07-a160-43d7-845b-66e9462a8bd2
  collapsed:: true
	- Given a closed **Contour** we can check whether it contains any other contour by building a **[Polygon](https://docs.rs/geo/0.33.1/geo/geometry/struct.Polygon.html)** from its **ls** and testing point-in-polygon containment directly against every other closed contour, using the **[Contains](https://docs.rs/geo/0.33.1/geo/algorithm/contains/trait.Contains.html)** trait from the **geo** crate. We only need one point per candidate contour (its first **Coord** is convenient), since a contour never crosses another contour: if one point of it is inside, the whole contour is.
	  ```rust
	  use geo::{Coord, LineString, Polygon};
	  use geo::algorithm::Contains;
	  
	  /// Builds a Polygon from a closed contour's LineString (no holes).
	  fn contour_polygon(c: &Contour) -> Polygon<f64> {
	      Polygon::new(c.lwg.ls.clone(), vec![])
	  }
	  
	  /// Returns true if the closed contour at `candidate_idx` encloses at least
	  /// one other closed contour.
	  fn encloses_another_contour(candidate_idx: usize, contours: &[Contour]) -> bool {
	      debug_assert!(contours[candidate_idx].lwg.ls.is_closed());
	  
	      let candidate_poly = contour_polygon(&contours[candidate_idx]);
	  
	      contours.iter().enumerate().any(|(i, c)| {
	          i != candidate_idx
	              && c.lwg.ls.is_closed()
	              && candidate_poly.contains(&c.lwg.ls.0[0])
	      })
	  }
	  ```
- ## Appendix 4
  id:: 6aa157fd-862a-4e2d-9b2c-2184a96494cc
  collapsed:: true
	- Given the previous and current position of a **rain drop** (a step of length **rain_drop_step**, typically several pixels long), we need to check every pixel of the **Contour Raster** that the segment between them actually touches, not just the pixel at either endpoint — otherwise a thin (single-pixel-wide) rasterized contour can be stepped over without being detected. Converting `prev`/`next` to their own pixel indices first and walking a supercover line traversal between *those two cells* (e.g. the [line_drawing](https://crates.io/crates/line_drawing) crate's `Supercover` iterator) is not enough: it only ever sees which cell each endpoint floors into, never where within that cell it actually sits, nor the continuous line's real path between them — a thin, diagonally-placed contour can clip a multi-pixel-long step for a fraction of its length without either endpoint's own pixel being anywhere near it, and that crossing is missed entirely. Instead we walk the continuous segment directly in pixel space: a standard grid-traversal/DDA algorithm (Amanatides–Woo style) that tracks the exact parametric position at which the segment crosses each vertical or horizontal pixel boundary, so every cell the line geometrically touches is found regardless of how long the step is or where exactly within their own pixels the endpoints fall. A step that passes exactly through a shared corner of four pixels visits both cells flanking that corner (not just the two diagonal ones), the same conservative convention a supercover traversal itself uses for a corner crossing.
	- The rain drop's own **source contour** must be excluded from this check (its index is carried alongside the drop for its whole lifetime), otherwise the drop would evaporate against its own origin on its very first step. Excluding it here, at the traversal level, is where that exception belongs rather than downstream in the Rain Drop Production logic.
	- A pixel outside the raster's own array bounds is treated the same as a pixel that holds `1` (out of bound) — leaving the map is, semantically, exactly what the out-of-bound value already means, so there's no separate bounding-box check needed before a step.
	- The function returns the first of the following it finds along the step: an out-of-bound (`1`) pixel (including stepping off the array entirely), a high-density (`3`) pixel, or a contour (**CONTOUR_0_MATRIX_VALUE** or above, other than the excluded source) — or reports the step as clear if none of these occurs before reaching `next`. What the caller does with that result (evaporate, vote and continue, mark a pixel, and so on) is decided at the caller — see the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c) — this appendix only answers "did this step cross something, and what."
	- ```rust
	  use geo::Coord;
	  
	  /// What the first touched pixel along a step turned out to be.
	  enum StepHit {
	    Contour(u64),
	    OutOfBound,
	    HighDensity,
	  }
	  
	  /// Value at and above which a Contour Raster pixel means "contour (value -
	  /// CONTOUR_0_MATRIX_VALUE)". See Step 1.
	  const CONTOUR_0_MATRIX_VALUE: u32 = 5;
	  const OUT_OF_BOUND: u32 = 1;
	  const HIGH_DENSITY: u32 = 3;
	  
	  /// Calls `visit` with every pixel the segment `prev -> next` touches, in
	  /// the order visited, stopping early if `visit` returns `false`. Tracks
	  /// the exact parametric position (`t`, 0 at `prev`, 1 at `next`) at which
	  /// the segment next crosses a vertical or horizontal pixel boundary, and
	  /// steps into whichever cell that crossing leads to -- both, if it
	  /// passes exactly through a shared corner, the way a supercover
	  /// traversal is itself conservative about corner touches.
	  fn walk_pixels(
	    prev: Coord<f64>,
	    next: Coord<f64>,
	    origin: Coord<f64>,
	    rasterization_px_size: f64,
	    mut visit: impl FnMut(i64, i64) -> bool,
	  ) {
	    let fx0 = (prev.x - origin.x) / rasterization_px_size;
	    let fy0 = (prev.y - origin.y) / rasterization_px_size;
	    let fx1 = (next.x - origin.x) / rasterization_px_size;
	    let fy1 = (next.y - origin.y) / rasterization_px_size;
	    let (dx, dy) = (fx1 - fx0, fy1 - fy0);
	  
	    let (mut x, mut y) = (fx0.floor() as i64, fy0.floor() as i64);
	    let (end_x, end_y) = (fx1.floor() as i64, fy1.floor() as i64);
	  
	    if !visit(x, y) { return; }
	    if x == end_x && y == end_y { return; }
	  
	    let step_x = if dx > 0.0 { 1 } else if dx < 0.0 { -1 } else { 0 };
	    let step_y = if dy > 0.0 { 1 } else if dy < 0.0 { -1 } else { 0 };
	    let t_delta_x = if dx != 0.0 { (1.0 / dx).abs() } else { f64::INFINITY };
	    let t_delta_y = if dy != 0.0 { (1.0 / dy).abs() } else { f64::INFINITY };
	    let mut t_max_x = if dx > 0.0 { ((x + 1) as f64 - fx0) / dx }
	      else if dx < 0.0 { (x as f64 - fx0) / dx } else { f64::INFINITY };
	    let mut t_max_y = if dy > 0.0 { ((y + 1) as f64 - fy0) / dy }
	      else if dy < 0.0 { (y as f64 - fy0) / dy } else { f64::INFINITY };
	  
	    const CORNER_EPSILON: f64 = 1e-9;
	    loop {
	      if (t_max_x - t_max_y).abs() < CORNER_EPSILON {
	        // Passes exactly through the shared corner of four cells: touches
	        // both flanking cells before the diagonal one.
	        if !visit(x + step_x, y) { return; }
	        if !visit(x, y + step_y) { return; }
	        x += step_x; y += step_y;
	        t_max_x += t_delta_x; t_max_y += t_delta_y;
	      } else if t_max_x < t_max_y {
	        x += step_x; t_max_x += t_delta_x;
	      } else {
	        y += step_y; t_max_y += t_delta_y;
	      }
	      if !visit(x, y) { return; }
	      if x == end_x && y == end_y { return; }
	      if t_max_x > 1.0 && t_max_y > 1.0 { return; } // reached next
	    }
	  }
	  
	  /// Walks every pixel the segment `prev -> next` traverses in the Contour
	  /// Raster and returns the first of an out-of-bound pixel, a high-density
	  /// pixel, or a contour other than `exclude_contour_idx`, whichever comes
	  /// first -- or None if the step is clear.
	  fn step_traversal_hit(
	    prev: Coord<f64>,
	    next: Coord<f64>,
	    origin: Coord<f64>,
	    rasterization_px_size: f64,
	    contour_raster: &Vec<Vec<u32>>,
	    exclude_contour_idx: u64,
	  ) -> Option<StepHit> {
	    let mut hit = None;
	    walk_pixels(prev, next, origin, rasterization_px_size, |x, y| {
	      if x < 0 || y < 0 {
	        hit = Some(StepHit::OutOfBound);
	        return false;
	      }
	      let (ux, uy) = (x as usize, y as usize);
	      let Some(row) = contour_raster.get(uy) else {
	        hit = Some(StepHit::OutOfBound);
	        return false;
	      };
	      let Some(&val) = row.get(ux) else {
	        hit = Some(StepHit::OutOfBound);
	        return false;
	      };
	  
	      if val == OUT_OF_BOUND {
	        hit = Some(StepHit::OutOfBound);
	        return false;
	      }
	      if val == HIGH_DENSITY {
	        hit = Some(StepHit::HighDensity);
	        return false;
	      }
	      if val < CONTOUR_0_MATRIX_VALUE {
	        return true; // 0 (undefined) or 2 (no contour, in bound): keep walking
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
	  ```
- ## Appendix 5
  id:: 6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e
  collapsed:: true
	- Given a Flying End's current position (see Step 1's Growing Process) and the `contours_step`-away node case (a)/(b) didn't already resolve, this computes where the next grown node should go.
	- For every pixel with value `1` or `3` inside the attraction window (**growing_window_size_px_attractions**), or **TEMPORARY_CONTOUR**/**CONTOUR_0_MATRIX_VALUE**-or-above inside the contour window (**growing_window_size_px_contours**), we take its distance and unit vector from the Flying End, and weigh it: `1` and `3` pixels pull toward themselves (positive weight), any contour pixel -- **TEMPORARY_CONTOUR** included, classified here exactly like a real contour pixel -- pushes away (negative weight) — including the growing contour's own trailing pixels, which sit directly behind the Flying End, so "push away from them" already means "keep moving forward," and including any *other* Flying End's own not-yet-final tail (also **TEMPORARY_CONTOUR** until it resolves), so two Flying Ends growing at the same time repel each other's tails instead of only reacting to already-finalized contours. Blended with the direction the contour was already heading in, this gives the next step's direction; the new node is placed exactly **growing_step_length * contours_step** away along it. If the window holds none of these pixels at all, the direction defaults to the contour's own previous heading, unchanged. During the Growing Process's **Matching** pass (Step 1), `1` pixels are skipped entirely here — `seeking` (below) is `false` — rather than merely zero-weighted, so a Flying End that's exhausted its **growing_oob_seeking_max_steps** budget stops being pulled toward the border at all, and settles into matching a nearby Flying End instead.
	  ```rust
	  use geo::Coord;
	  
	  enum WindowPixelKind {
	    OutOfBound,
	    HighDensity,
	    Contour,
	  }
	  
	  /// One pixel found in this kind's own window (attraction or contour,
	  /// depending on `kind`), already classified, with its world-space
	  /// center.
	  struct WindowHit {
	    kind: WindowPixelKind,
	    center: Coord<f64>,
	  }
	  
	  fn unit(dx: f64, dy: f64) -> Coord<f64> {
	    let len = (dx * dx + dy * dy).sqrt();
	    Coord { x: dx / len, y: dy / len }
	  }
	  
	  /// The (not yet normalized) direction the Growing Process's next
	  /// `contours_step`-long move should take from `flying_end`, given
	  /// `previous_direction` (the unit vector of the contour's own last
	  /// segment) and every attractive/repulsive pixel found in the window.
	  /// `seeking` is `true` while this Flying End is still in the Growing
	  /// Process's **Seeking** pass, `false` once it has fallen through to
	  /// **Matching** -- see Step 1's Growing Process.
	  fn growing_direction(
	    flying_end: Coord<f64>,
	    previous_direction: Coord<f64>,
	    window_hits: &[WindowHit],
	    seeking: bool,
	    growing_previous_distance_direction_weight: f64,
	    growing_out_of_bound_direction_weight: f64,
	    growing_density_direction_weight: f64,
	    growing_other_contours_direction_weight: f64,
	  ) -> Coord<f64> {
	    let mut dir = Coord {
	      x: growing_previous_distance_direction_weight * previous_direction.x,
	      y: growing_previous_distance_direction_weight * previous_direction.y,
	    };
	    for hit in window_hits {
	      if !seeking && hit.kind == WindowPixelKind::OutOfBound {
	        continue; // dropped once a Flying End moves on to Matching
	      }
	      let (dx, dy) = (hit.center.x - flying_end.x, hit.center.y - flying_end.y);
	      let d = (dx * dx + dy * dy).sqrt();
	      if d == 0.0 {
	        continue; // degenerate: the pixel sits exactly on the Flying End
	      }
	      let b = unit(dx, dy);
	      let w = match hit.kind {
	        WindowPixelKind::OutOfBound => growing_out_of_bound_direction_weight,
	        WindowPixelKind::HighDensity => growing_density_direction_weight,
	        WindowPixelKind::Contour => growing_other_contours_direction_weight,
	      };
	      dir.x += w * b.x / d;
	      dir.y += w * b.y / d;
	    }
	    dir
	  }
	  
	  /// The next grown node: `step` (the caller's own `growing_step_length *
	  /// contours_step`) away from `flying_end` along `direction`
	  /// (`growing_direction`'s own result), or straight ahead along
	  /// `previous_direction` if `direction` came out zero (the window held
	  /// nothing to react to at all).
	  fn next_grown_node(
	    flying_end: Coord<f64>,
	    previous_direction: Coord<f64>,
	    direction: Coord<f64>,
	    step: f64,
	  ) -> Coord<f64> {
	    let len = (direction.x * direction.x + direction.y * direction.y).sqrt();
	    let d = if len == 0.0 { previous_direction } else { unit(direction.x, direction.y) };
	    Coord {
	      x: flying_end.x + d.x * step,
	      y: flying_end.y + d.y * step,
	    }
	  }
	  ```
