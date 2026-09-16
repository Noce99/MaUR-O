- This is an algorithm description that should, given some contours and other terrain objects from a .omap, return a raster of the given map with, for each pixel, the elevation value up to a constant with respect to the real ones.
- ## Assumptions
  collapsed:: true
	- 1) This algorithm assumes that the gravity of the same contour line is always going in the same direction. In other words, it is not possible to find a contour line with slope lines pointing toward different parts of the same contour line.
	- The form lines are completely ignored
- ## Parameters
	- **bezier_linearization_step** (meters) — maximum chord length used to flatten a cubic Bezier segment into straight lines ([Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)). Should be noticeably smaller than **contours_step**: it runs first, and the curve detail it captures is exactly what the equal-chord resampling with **contours_step** could otherwise erase from a curve if the two were comparable.
	- **contours_step** (meters) — the equally-spaced node distance every contour's final `ls` is resampled to ([Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)). This is the working resolution the rest of the algorithm (source placement, circle fitting) operates on.
	- **rasterization_px_size** (meters) — pixel size of the Contour Raster (Step 1). The finest positional resolution the whole algorithm can resolve; it must be small enough that no two genuinely distinct contours ever land in the same pixel, since a conflict there is marked as high density rather than resolved to either contour — in practice it is bounded by the tightest contour bunching expected on the map (e.g. at cliffs).
	- **heavy_object_width** (meters, `width` in [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e)) — buffer width used to turn a **Jump**'s or a **Heavy Object**'s own `ls` into a polygon. For a Jump, this is the area Step 2 scans to find which contours it touches; for a Heavy Object, it is the area Step 1 itself searches for an intersecting contour, instead of only the pixels directly under its digitized line. Should be at least a couple of **rasterization_px_size**, so the buffered polygon covers a real ring of pixels rather than collapsing into a single pixel row.
	- **heavy_object_growing** (meters, `extra_growing` in [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e)) — extra padding buffered on top of **heavy_object_width**. Normally much smaller than **heavy_object_width** itself — a small safety margin, not a second width.
	- **circumference_fitting_points_number** (pure number) — how many **Coord**s on each side of a Heavy Object/contour intersection are used to fit a circle (Step 1). Should be a small multiple of the point density set by **contours_step**: enough points for a stable fit (at least 3), but few enough that the sampled arc still reflects local curvature at the intersection rather than the contour's shape further away.
	- **slope_lines_contours_search_radius** (meters) — how far around a Slope Line's own position Step 1 searches for the nearest Contour Raster pixel to attribute its gravity reading to. A Slope Line's placement on the map is not always pixel-exact on top of its own contour, so a plain under-the-point lookup misses legitimate readings; too small and this still happens, too large and a reading risks being attributed to the wrong, merely-nearby contour instead.
	- **rain_drop_step** (meters) — distance a rain drop advances per simulation step, shared by every variant of the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c). Should stay small relative to **rasterization_px_size** (a few pixels at most); [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc)'s pixel walk still finds every pixel a step crosses even for a large step, but a coarse step blurs exactly where along the path a vote or an evaporation was triggered and raises the chance of overshooting the map boundary or the source's own contour.
	- **sources_per_contour_segment** (pure number) — how many sources are placed per contour segment, shared by every variant of the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c). Should scale with how long segments are relative to **rain_drop_step**: the larger **contours_step** is, the higher **sources_per_contour_segment** needs to be to keep sources close enough together to resolve nearby thin contours individually.
	- **rain_drop_starting_voting_hysteresis** (pure number, counted in **rain_drop_step**-sized steps) — Cold-only: a Cold rain drop is exempt from evaporating on re-crossing its own starting contour within this many steps of being created, and, separately, exempt from evaporating on re-crossing any other contour it has already voted for within this many steps of *that vote* (see the [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)). Should be just large enough (a handful of steps) to carry the drop clear of either; since each exemption is measured from its own reference point (creation, or the vote itself), it does not need to be sized relative to the distance to neighboring contours or to how far into the drop's path a vote happens to fall.
	- **undefined_gravity_vote_threshold** (pure number, a ratio in (0, 1)) — Cold-only: how close left and right vote counts must be before being flagged as ambiguous (Step 3). A value near 1 only flags near-exact ties; a value near 0 flags votes on both sides however lopsided.
	- **obvious_to_close_contour_distance** (meters) — the Growing Process's own preliminary Close Search pass's own "obvious match" distance (Step 1's Growing Process): any two Flying Ends closer to each other than this are matched outright, before Close Search's own cone-based searches (below) ever run and regardless of either one's own forward direction -- close enough that which way either one happens to be pointing doesn't matter. Still subject to the same crossing check every other Flying-End candidate is. Should stay small, a handful of meters at most, since it deliberately ignores direction entirely. `0` turns this particular check off outright, the same convention as **growing_oob_seeking_max_steps**'s own `0`.
	- **searching_fov** (degrees) — the Growing Process's own preliminary Close Search pass's own cone's total angular width, centered on a Flying End's own forward direction (Step 1's Growing Process): half this angle to each side of that direction. Together with **searching_distance**, bounds the area Close Search scans, first for another Flying End, then for an out-of-bound pixel, then for a high-density pixel, before Seeking ever takes a single integration step.
	- **searching_distance** (meters) — the same pass's own max search radius, for all three of its cone-based searches. `0` turns those three off outright, the same convention as **growing_oob_seeking_max_steps**'s own `0` (**obvious_to_close_contour_distance**'s own check is independent, and stays on unless it is itself `0`). Should stay small: this pass exists to catch Flying Ends that are already essentially where they need to be, not to replace Seeking/Matching's own, farther-reaching physics.
	- **growing_oob_seeking_max_steps** (pure number, a non-negative integer) — how many integration steps a Flying End spends in the Growing Process's first pass, seeking the out-of-bound area entirely on its own (matching against another Flying End disabled outright, along with **flying_end_force**), before falling through to the second pass (matching restored, **out_of_bound_force** dropped) for its remaining steps (Step 1's Growing Process). `0` turns the first pass off outright — every Flying End starts directly in the second. Should be a handful of steps: enough to actually reach a border that's genuinely close, not so many that a Flying End with none nearby wastes a long detour before it gets to try matching instead.
	- **contour_force_window** (meters) — the Growing Process's own scan radius for contour-pixel force (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): a real contour pixel or a **TEMPORARY_CONTOUR** tail only contributes its own contour force if its Euclidean distance from the Flying End is within this radius -- except the Flying End's own pixel and its 8 immediate neighbors, always excluded regardless of this setting, since the Flying End always sits right on top of its own just-written body there, and that self-proximity would otherwise swamp the force with a huge, meaningless push instead of reflecting genuinely nearby contour pixels. Should reach comfortably past **contour_force_second_equilibrium**, or a contour pixel drifting distant enough to need that far term never gets the chance to feel it.
	- **attraction_force_window** (meters) — the same, for the three constant-magnitude forces (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): an out-of-bound pixel, a high-density pixel, or another pending Flying End to match against, only contributes its own force (**out_of_bound_force**/**density_region_force**/**flying_end_force**) if its Euclidean distance from the Flying End is within this radius. Kept separate from **contour_force_window** so how far the Growing Process reaches for something to move toward can be tuned independently of how far it reaches for something to repel it. Every qualifying pixel/end within range contributes (summed, not just the nearest), so a wider window over a long run of out-of-bound pixels (a whole border, say) pulls harder than a narrow one would; **out_of_bound_force**/**density_region_force** contribute their own full magnitude regardless of distance within the window, but **flying_end_force** does not -- see its own entry.
	- **contour_force_max_repulsion** (Newtons, positive, `cfmr` below) — the contour-pixel force curve's own repulsion magnitude at zero distance (Step 1's Growing Process, [Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): a contour pixel right on top of the Flying End would push it away this hard.
	- **contour_force_equilibrium** (meters, positive, `cfe` below) — the contour-pixel force curve's own equilibrium distance ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): the distance at which a single contour pixel's own force is exactly zero. Closer than this, it repels (**contour_force_max_repulsion** at distance zero, fading to zero here); farther than this, it gently attracts instead, growing toward **contour_force_max_attraction** as the distance approaches **contour_force_second_equilibrium**.
	- **contour_force_max_attraction** (Newtons, negative, `cfma` below) — the contour-pixel force curve's own attraction magnitude at **contour_force_second_equilibrium** and beyond ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): a contour pixel farther than **contour_force_equilibrium** gently pulls the Flying End back toward it, up to this magnitude, so a Flying End doesn't drift arbitrarily far from a contour it's meant to stay loosely leashed near.
	- **contour_force_second_equilibrium** (meters, positive, always greater than **contour_force_equilibrium**, `cfse` below) — the contour-pixel force curve's own second equilibrium distance ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): the distance beyond which a single contour pixel's own attraction saturates at **contour_force_max_attraction** rather than continuing to grow.
	- **out_of_bound_force** (Newtons, positive) — the constant-magnitude attraction a single out-of-bound pixel within **attraction_force_window** contributes ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)). Summed over every out-of-bound pixel found, not just the nearest. Dropped entirely once a Flying End falls through to the Growing Process's second, matching pass.
	- **density_region_force** (Newtons, positive) — the same, for a single high-density pixel within **attraction_force_window** ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)). Summed over every high-density pixel found. Only applies during the Growing Process's second, matching pass, same as **flying_end_force** -- dropped entirely during the first, alongside **out_of_bound_force**'s own opposite gating.
	- **flying_end_force** (Newtons, positive) — the same, for another pending Flying End within **attraction_force_window** ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)), but unlike **out_of_bound_force**/**density_region_force** this one is this magnitude only at zero distance, falling off to zero at **attraction_force_window** itself (`flying_end_force * (1 - (d / attraction_force_window)^3)`) rather than staying constant across the whole window. Summed over every such end found. Only applies during the Growing Process's second, matching pass -- dropped entirely during the first, same as matching itself. The falloff exists because a crowded cluster of several Flying Ends converging at once otherwise lets a handful of more distant ones outvote the one actually about to merge, undamped and at full strength regardless of how close it already is -- and since which ends are still in range shifts step to step as some resolve, the net pull's own direction can swing sharply between two consecutive steps, leaving a sharp, physically implausible kink in the merged contour right at the join (see the Matching pass's merge, below) that can, in turn, read as a genuine gravity conflict in Step 2 even though the two contours agree about which side is downhill. Cubing the normalized distance (rather than a plain linear falloff) keeps the pull close to full strength across most of the window and only rolls it off sharply right near the edge, so two ends starting out far apart still get a meaningful pull from their very first integration step instead of crawling for many steps before the pull becomes noticeable -- while still landing on exactly `0` right at **attraction_force_window**, so the boundary discontinuity above doesn't come back.
	- **flying_end_merge_distance** (meters) — the Euclidean distance below which two pending Flying Ends actually merge (Step 1's Growing Process, matching pass only): either closing one contour into a ring (its own two ends) or splicing two separate contours together. **flying_end_force** only pulls two Flying Ends toward each other; this distance is what actually finalizes the merge once they're close enough.
	- **matching_min_force** (Newtons, non-negative) — the smallest net force a Matching pass integration step is allowed ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): a nonzero net force weaker than this (**density_region_force**/**flying_end_force** combined -- the only two terms Matching ever sees) is scaled up to this magnitude, direction unchanged, before becoming a displacement. Without it, two Flying Ends near the far edge of each other's **attraction_force_window** -- where **flying_end_force**'s own falloff is weakest -- or forces that partly cancel against a third nearby end, can crawl toward a merge over an impractically large number of integration steps. Seeking is unaffected: its own **growing_oob_seeking_max_steps** budget already bounds it, and a weak contour-pixel pull there genuinely means little is nearby to react to, not something to force along faster. Should stay smaller than **flying_end_force**/**density_region_force** themselves, so it only kicks in for a genuinely weak residual pull rather than becoming a dominant force in its own right. `0.0` turns it off outright.
	- **grow_time_step** (seconds) — how many seconds of simulated time each Growing Process integration step advances by ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): a Flying End's displacement each step is its net force (Newtons, used directly as meters/second -- no mass, no inertia) times this. `1.0` unless there is a specific reason to change it.
	- **growing_visualization_push_pull_vectors_scale** (pure number) — purely a visualization knob: how much the Growing Process's own two SVGs, `02_<map_name>_step1_growing_seeking.svg` and `03_<map_name>_step1_growing_matching.svg` (see Visualization, below), scale the four force vectors each draws for its own integration steps before drawing them, so their length is legible against the map's own scale. Never affects the Growing Process itself.
- ## Rain Drop Production Definition
  id:: 6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c
  collapsed:: true
	- A **Rain Drop Production** simulates a set of particles ("rain drops") walking away from a set of contours, to sense what lies around them. Every variant used elsewhere in this document — in Step 1, Step 3, and Step 4 — is one of four combinations of two independent choices, **Direction** and **Temperature**. This section defines what all four share; what a specific call site does *at* an evaporation (vote for a gravity side, mark a pixel, assign an elevation) is documented at that call site, not here — exactly as [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc) itself only answers "did this step cross something, and what," leaving what the caller does about it to the caller.
	- **Source placement** (shared by all four variants): given a contour, for each segment of its **LineString** we create **sources_per_contour_segment** **sources**, equally spaced. If **sources_per_contour_segment** is 3 we create a source at A, A + 0.33\*(B-A) and A + 0.66\*(B-A), where A and B are the starting and ending **Coord**s of the segment.
	- **Source direction** (shared by all four variants): each source's own direction (perpendicular to the contour, in accordance with its gravity — real gravity for a contour that already has one, or a call site's own placeholder otherwise, as Step 1 does) is a blend of the direction *at* node A and the direction *at* node B, weighted by the same fraction used to place that source along the segment -- the source at A is 100% A's own direction, the one at A + 0.33\*(B-A) is 66% A's direction and 33% B's, and so on, rather than every source along a segment leaving in the same, flat direction. A node's own direction is the mean of the perpendicular of the segment before it and the one after (a node with only one neighboring segment -- an open contour's first or last node -- takes that one alone; a closed contour's first node wraps its "previous" segment around to the contour's own last one). The direction of each rain drop never changes once it starts.
	- **Direction — Rain vs Anti Rain**: a **Rain Drop** moves in its source's own direction; an **Anti Rain Drop** moves in exactly the opposite direction. Nothing else differs between the two.
	- **Stepping and hit detection** (shared by all four variants): we let each rain drop step **rain_drop_step** meters at a time. To check what a step crosses, see [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc), which reports the first of: a specific contour (by index), an out-of-bound (`1`) pixel, or a high-density (`3`) pixel that the step touches, or reports the step as clear if none of these occurs. A rain drop's own source contour never counts as a contour hit against itself, for its whole lifetime (Appendix 4) — otherwise it would evaporate against its own origin on its very first step.
	- **Temperature — Cold vs Hot** (governs only when a drop evaporates; independent of Direction):
		- A **Cold** rain drop evaporates when it crosses an out-of-bound (`1`) pixel, a high-density (`3`) pixel, or a contour whose gravity is already defined — except within **rain_drop_starting_voting_hysteresis** steps of its own creation if that contour is its own starting one, or within **rain_drop_starting_voting_hysteresis** steps of having voted for a given contour if it's crossing that same contour again; past either window, crossing it does evaporate the drop. Note that the hysteresis exemptions only ever apply to a contour crossing, never to a high-density pixel — a Cold rain drop evaporates on high density unconditionally, even within its own starting window.
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
	- The Contour Raster's fill starts in three steps:
		- 1) Every pixel starts at `0` (undefined).
		- 2) For each contour, for each consecutive pair of **Coord**s of its **ls**, we walk every pixel that the segment between them touches using the pixel walk described in [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc) (the source-contour exclusion from that appendix does not apply here — every touched pixel is written the same way), writing **CONTOUR_0_MATRIX_VALUE** plus the contour's index. If a touched pixel already holds that same value, it's a no-op. If it's `1` (out of bound), it's left alone — that's the map's own edge, not another contour, and a contour's own last segment can legitimately run right up to (and through) one on purpose (see the Growing Process below); overwriting it, with either the contour's own value or `3`, would misreport genuine out-of-bound territory as something it isn't. If it's **TEMPORARY_CONTOUR**, it's free to claim too, exactly like `0` or `2` -- that's precisely how a Flying End's own tail, marked temporary one grow step at a time, becomes real once that contour finally resolves (see the Growing Process). If it already holds any other *different* value — another contour's, or already `3` — we set it to `3` (high density) instead and keep going: real contour digitizing sometimes places two physical lines closer together than **rasterization_px_size** can tell apart (e.g. at cliffs), and marking the pixel high density rather than crashing is how the rest of the algorithm is told not to trust it as belonging to any one contour.
		- 3) For each contour, we run one **Hot Rain Drop Production** and one **Hot Anti Rain Drop Production** (see [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)) — since no contour has a real gravity direction yet at this point, either perpendicular side is used as a throwaway placeholder "gravity" for this purpose only: a Rain Drop Production and its Anti Rain counterpart together always cover both perpendicular sides of a contour regardless of which side got the placeholder label, so the choice can't affect the result. Each drop accumulates, across every step of its own path, every pixel that step's own Appendix 4 walk touches while still `0` — but only actually sets them to `2` once the drop itself evaporates, and only if it evaporates by hitting a contour or a high-density pixel, *not* by leaving the map. A drop that leaves the map must not have any of its path committed: doing so would plant a stretch of `2` pixels between the map's real out-of-bound area and its own border, which the out-of-bound computation below can only ever flood-fill through `0` pixels — those pixels would be stuck showing "in bound" when they are really outside the map.
	- All the other symbols (**Slope Lines**, **Jumps** and **Heavy Objects**) are just used to understand the gravity direction and are stored inside a **Vector** of instances of **LineGravityDefiners** and a **Vector** of **PointGravityDefiners**. **Slope Lines** and **Jumps** are resolved right away, in the following way (**Heavy Objects** are resolved separately, only after the Growing Process below has finished -- see that bullet, further down, for why):
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
	- We compute out of bound, only now that every Jump's own area has already been stamped `3` above -- a Jump close to the map's own border must already act as a firebreak here, or the flood below would leak straight through its (otherwise still-undefined) pixels into the map's interior before the Jump ever got a chance to block it:
		- Every pixel on the raster's own border (its first and last row, first and last column) is set to `1`.
		- We then flood outward 8-connectedly from there: an **active_out_of_bound_pixels** list starts as every pixel currently `1`. Each iteration, for every pixel in that list, we check its 8 neighbours; any neighbour currently `0` is set to `1` and collected into a fresh **active_out_of_bound_pixels** list for the next iteration. We stop once an iteration collects nothing. Since this only ever turns a `0` into a `1`, it can never eat into a contour, high-density, or no-contour-in-bound pixel -- those act as a firebreak, which is exactly how a `0` pixel fully enclosed by them can legitimately stay `0` (undefined) forever.
	- **Closing dangling ends: the Growing Process.** After every other Step 1 sub-step above has run — including Slope Lines and Jumps, so the Contour Raster's out-of-bound and high-density values are final -- every open contour's start and end node should resolve to an out-of-bound (`1`) or high-density (`3`) pixel (closed contours have no ends, so they're exempt).
		- We first build the list of **Flying Ends**: every open contour's start or end node whose own Contour Raster pixel is neither `1` nor `3`.
		- Before any physics runs, every Flying End is offered a preliminary **Close Search** pass: a cheap, non-iterative geometric check, not a simulation step, meant to catch a Flying End that is already essentially where it needs to be without spending any integration steps on it. It runs in two passes of its own, **discovery** then **resolution**, rather than resolving each Flying End the moment a match for it is found -- a Flying End's own merely-okay match must not be free to claim a shared candidate before a genuinely closer match to that same candidate, discovered elsewhere, ever gets a chance; which candidate is *best* should decide the outcome, not which Flying End happened to be looked at first.
			- **Discovery**: first, every *pair* of Flying Ends closer to each other than **obvious_to_close_contour_distance** is recorded as a match candidate outright -- too close for either one's own forward direction to matter, so no cone or direction check applies here at all, only the same crossing check the cone-based search below uses (a third contour physically between two close Flying Ends still blocks the match). Every such pair is recorded, not just each Flying End's own single closest one; resolution's own global ascending-distance order (below) already sorts the closest ones to the front. Then, for every Flying End, in turn, a cone directly in front of it is searched -- apex at its own current position, opening along its own forward direction (its last segment, extrapolated past its own tip), **searching_fov** degrees wide in total (half that angle to each side of the forward direction), reaching out to **searching_distance** meters -- for its own single best (closest valid) match, and that match is recorded (candidate Flying End, out-of-bound pixel, or high-density pixel, together with its own distance) rather than resolved right away. Every Flying End is searched against the same, original, still entirely unresolved set every other one is -- nothing about any Flying End's own position or contour changes during discovery, so where in the list a Flying End falls makes no difference to what it finds; a Flying End can end up with more than one recorded candidate this way (an obvious-match one and a cone-based one), which resolution below sorts across together regardless. Three searches run inside that cone, in order:
				- First, for another Flying End. Every other Flying End inside the cone is a candidate; a candidate is valid only if the straight segment from this Flying End to it does not cross a third contour -- checked with [Appendix 4](6aa157fd-862a-4e2d-9b2c-2184a96494cc)'s own hit detection (excluding this Flying End's own contour): clear, reaching only the candidate's own contour, or crossing out-of-bound territory along the way (the empty/unclaimed space directly between two close Flying Ends is expected, and has nothing to do with another contour being there), is valid; reaching high-density (an unresolved conflict between contours, not open space) or a *different* contour first is not. Among every valid candidate, the closest one is recorded.
				- Otherwise -- no valid Flying End found -- the same cone is searched a second time, this time for an out-of-bound (`1`) pixel, with the same validity rule (via Appendix 4, only an out-of-bound pixel reached before anything else counts) and the same closest-valid-candidate choice. If one is found, it is recorded instead.
				- Otherwise -- neither of the first two searches found anything -- the same cone is searched a third time, this time for a high-density (`3`) pixel, with the same validity rule (via Appendix 4, only a high-density pixel reached before anything else counts) and the same closest-valid-candidate choice. High density is exactly as valid a resolution for a Flying End as out-of-bound is -- a Flying End is only ever "still flying" while its own pixel is neither (Step 1's Contour Raster construction) -- it is only tried third here because reaching the map's own edge is the more common, and generally more meaningful, way for a contour to end. If one is found, it is recorded instead.
				- A Flying End for which none of the three searches finds anything gets no candidate recorded at all, and is left for Seeking to pick up later (or, if `searching_fov`/`searching_distance` leaves the cone empty, every Flying End goes through discovery finding nothing, and Close Search is effectively off).
			- **Resolution**: every candidate recorded during discovery, of any of the three kinds, is applied in ascending distance order -- the single globally closest one first, then the next closest, and so on -- until none remain. Applying a Flying-End candidate closes its own contour into a ring exactly as **Matching**'s own merge check does (below) if the two Flying Ends belong to the same contour, or splices the two contours together exactly as **Matching**'s own merge check does otherwise; either way both Flying Ends involved are resolved, and skip Seeking and Matching entirely. Applying a landing candidate (out-of-bound or high-density, handled identically once found) resolves that one Flying End directly onto it -- the same new-node-then-resample-then-redraw step an ordinary integration step's own landing uses, below -- and it alone skips Seeking and Matching. Before a candidate is applied, though, it is checked against every Flying End already resolved by an earlier (closer) candidate in this same pass: if either Flying End it names (both, for a Flying-End candidate) has already been resolved, this candidate is stale -- it was only ever valid against the *original*, discovery-time set, and that Flying End no longer exists as such -- and is discarded unapplied, never re-searched for a fresh partner. A merge additionally renumbers every still-unapplied candidate's own contour reference onto the merged contour's new shape (the same remapping **Matching**'s own merge check performs on its own still-pending Flying Ends, below), so a later candidate that named one of the two just-merged contours' own *other*, uninvolved end still resolves correctly rather than against a stale or now-out-of-range index.
		- Every Flying End still unresolved after Close Search then runs the Growing Process's own physics simulation: a real, overdamped simulation, not a heuristic step -- every pixel or other Flying End within reach exerts a genuine, Newton-valued force on it (contour pixels through a potential-well curve, everything else a constant magnitude), the forces sum into one net force, and that net force *is* the Flying End's velocity (no mass, no inertia, nothing carried over from one integration step to the next -- a zero net force simply means no movement that step). Runs in two passes, **Seeking** then **Matching**:
			- **Seeking**: up to **growing_oob_seeking_max_steps** integration steps, entirely on its own -- merging against another Flying End is disabled outright, along with **flying_end_force** and **density_region_force**, leaving only the contour-pixel potential well and **out_of_bound_force** active: a border to reach, or an equilibrium distance from every nearby contour pixel, with neither another Flying End nor a high-density region getting a say yet. A Flying End that resolves within this budget is done; one that doesn't has every node this pass appended to it stripped back off its own `ls` (and every `TEMPORARY_CONTOUR` pixel those steps marked reset, so its own abandoned attempt can't go on attracting/repelling it) before falling through to **Matching**, which then starts fresh from that same Flying End's own pre-Seeking position rather than continuing on from wherever the matching-blind search left it. This exists because two contours that happen to run close and parallel near the border can land within reach of each other purely because they're both, independently, heading for that same border -- not because they're actually the same physical line split in two -- and without a matching-free pass first, merging would snap them together instead of letting each reach the border on its own, even when each already has a perfectly good border of its own close by; since **Matching**'s own physics are a completely different pair of terms, whatever path Seeking's own, different physics were tracing is not one Matching should simply pick up and continue.
			- **Matching**: merging (and **flying_end_force**) restored, along with **density_region_force**, but both **out_of_bound_force** and the contour-pixel potential well (**contour_force_***) dropped instead -- having already had its own dedicated, matching-free shot at both during Seeking, Matching leaves the Flying End to react only to another pending Flying End or a high-density region, settling into whichever it's actually closer to instead of still chasing a border it hasn't found or fighting its own trailing body. Each of the Growing Process's four force terms now belongs to exactly one phase: the contour-pixel potential well and **out_of_bound_force** to Seeking, **flying_end_force** and **density_region_force** to Matching -- none apply in both.
			- Both passes are round-robin rather than one Flying End at a time to completion: each still-unresolved Flying End takes exactly one integration step below, then the whole list is cycled through again, and so on until none are left in that pass (resolving one, in **Matching**, can also resolve another already in the list -- the merge check below). Each step of a Flying End's Growing Process, in either pass:
			- A new node is never drawn into the Contour Raster under its own contour's real index the moment it's added to a contour's `ls` -- only once that `ls` reaches the truly final shape it will keep, on a merge/close or a landing (below). A node placed by an ordinary integration step is, until then, recorded only in the contour's own `ls`; a later resample (closing or merging, or landing) can still shift nodes placed long before, not only the newest one, out of phase with whatever pixels their earlier position had already claimed. Whenever that happens, the contour's (both contours', for a merge) entire previous Contour Raster footprint is cleared first, then the new, final `ls` is drawn into it fresh, rather than drawn additively on top of the old.
			- That said, an ordinary integration step is not entirely invisible to the raster in the meantime: every pixel its own displacement crosses is immediately marked **TEMPORARY_CONTOUR** (see the tunneling-safe walk, below), so a *different* Flying End's own window scan, running its own turn a moment later, still repels off of it -- without this, two Flying Ends growing at the same time, each still with nothing drawn under a real index, could fly straight through each other. If that walk instead finds a pixel already holding a *different* contour's real value, or another Flying End's own **TEMPORARY_CONTOUR** tail, it is left exactly as it is -- never overwritten, and never turned into high density the way step 2 of the Contour Raster's own construction is -- and a warning is raised instead, naming the contour and the pixel; a step revisiting its *own* earlier trail is expected and stays silent. Every remaining **TEMPORARY_CONTOUR** pixel is swept back to `2` (no contour, in bound) once every Flying End in both passes has resolved, not per contour as each one resolves -- a still-flying neighbor may still be relying on that same pixel as a repeller.
			- **The merge check** (Matching only): if another pending Flying End's real, continuous position is closer than **flying_end_merge_distance**, the two merge -- **flying_end_force** only ever pulls two Flying Ends toward each other; this distance check is what actually finalizes a merge once they're close enough. If that other Flying End belongs to a *different* contour, the two contours are merged end to end (oriented so the two close ends meet), with the merged `ls` re-run through [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)'s equal-chord resampling, and one of the two contour indices is discarded at random, every higher index shifted down by one everywhere it's used -- the Contours vector, every Contour Raster pixel encoding a higher index, and every already-collected `PointGravityDefiners.reference_contour`. Either merged contour's *other* end, if it is itself still a pending Flying End waiting on its own later turn, is also remapped rather than simply shifted: it keeps following its own contour (now the kept index) and becomes that contour's new start or end -- whichever end this merge didn't just consume always ends up at the front of the merged `ls` for the kept contour and at the back for the discarded one, regardless of which of the two contours' own start/end nodes happened to be the ones that just met. If it instead belongs to the *same* contour -- its own other end, a valid and good match here, not a case to skip -- that contour is closed into a ring instead: the same new node closes it (its first and last `Coord` become the same point), and it is re-run through Appendix 1's equal-chord resampling the same way any closed contour is, shrinking the step to evenly divide the perimeter rather than leaving a short closing segment. Either way, the Growing Process is finished for both Flying Ends involved, and the resulting `ls` (or `ls`es) is (re)drawn into the Contour Raster as described above.
			- **Else, an integration step** ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)): the net force at the Flying End's current position is computed -- the contour-pixel potential well summed over every qualifying pixel within **contour_force_window**, plus (phase permitting) **out_of_bound_force**/**density_region_force**/**flying_end_force** summed over every qualifying pixel or end within **attraction_force_window** -- during Matching, if that net force is nonzero but weaker than **matching_min_force**, it is scaled up to that magnitude first, direction unchanged (see its own entry above) -- and the raw next position is the current one displaced by that net force times **grow_time_step**. Rather than only checking that raw next position, the real continuous path to it is walked pixel by pixel (the tunneling-safe walk, above): if an out-of-bound or high-density pixel is crossed anywhere along the way, the Growing Process finishes there, at that pixel's own center (not the raw next position, and not necessarily the nearest such pixel to the *start* of the step -- the first one actually crossed), and the `ls` -- now needing a resample, since the landing distance is whatever the crossing happened to be, not a fixed step length -- is re-run through Appendix 1's equal-chord resampling and (re)drawn into the Contour Raster as described above. Otherwise the raw next position becomes the Flying End's new position, nothing crossed along the way rewrites any real contour index, and the process repeats from there (in whichever pass it's still in).
	- **Heavy Objects, resolved after the Growing Process.** Unlike Slope Lines and Jumps above, a Heavy Object's own gravity reading is deliberately not resolved yet at that point: fitting a circle against a contour fragment that might still later be merged end to end into a longer, differently-shaped one -- right at that fragment's own dangling end, where few points are available for the fit (**circumference_fitting_points_number**'s own each-side points cannot reach past an open end) -- can disagree with a fit taken from the *other* fragment's own dangling end, even when the two fragments are genuinely the same, consistent hillside (see **flying_end_force**'s own entry above for the mechanism, and why it can leave a sharp kink right at a fresh merge's join). Resolving it only now, once the Growing Process immediately above has finished every merge and closure, means every contour's own geometry is truly final by the time each fit runs. The **Heavy Objects** are line symbols, so they should also first of all be transformed into a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** following [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8), then into a buffered polygon following [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e) (using the same **heavy_object_width**/**heavy_object_growing** parameters a Jump's own polygon uses) -- this buffering happens right away, alongside Slope Lines and Jumps above, since it depends only on the Heavy Object's own digitized line, not on any contour's geometry. The difference with respect to **Jumps** is that their gravity direction cannot be obtained from the .omap file directly, but needs to be inferred by considering the shape of the contours that intersect with them and the fact that they are not lines always perpendicular to gravity, but always parallel to it. We can still get information from them in the following way. We rasterize the buffered polygon (the same way a Jump's own polygon is, in Step 2) and, for each of its pixels with a value of **CONTOUR_0_MATRIX_VALUE** or above in the 2D Contour Vector, we have a candidate intersection with that contour (remember to subtract **CONTOUR_0_MATRIX_VALUE** from the value to get the index). Searching the whole buffered area, not just the pixels directly under the digitized line, catches a contour that runs close to a Heavy Object without landing exactly on it — the same motivation as **slope_lines_contours_search_radius** for Slope Lines. Since the polygon commonly covers several pixels of the very same nearby contour, we group its matched pixels by contour index and treat each distinct contour as a single intersection, at the centroid of that contour's own matched pixels within the polygon, rather than one intersection per pixel. For each such intersection we should compute the circumference that fits the contour in the section around it (the number of **Coord**s to use before and after the intersection for the fit is defined by a parameter, **circumference_fitting_points_number**), and we should define the gravity of that intersection based on the vector starting from the intersection and ending at the center of the fitted circumference. Now we have a point, we know the reference contour, and we also know the gravity direction, so we can append an item to the **Vector** of **PointGravityDefiners**.
- ## Step 2: Obvious Gravity Definition
  collapsed:: true
	- We scan all the items in the **Vector** of **PointGravityDefiners** and, for each one, we set the gravity direction of its reference contour if not already defined, or check whether it is in accordance with the already-set gravity otherwise. The algorithm should crash with an error explaining the problem if they do not match.
	- We scan all the items in the **Vector** of instances of **LineGravityDefiners** and, for each of its own **touched_contours** entries (not by re-rasterizing **poly** against the Contour Raster here -- by this point its own area is high density, not the contours it used to show, so nothing would be found), we set the gravity of that contour to the gravity of the **LineGravityDefiners** if it is not already defined, or check whether it is in accordance with the already-set contour gravity otherwise. The algorithm should crash with an error explaining the problem if they do not match.
	- We scan all the closed contours (we should check whether the LineString is closed) without a defined gravity direction. If they do not contain any other contours (see [Appendix 3](6aa14c07-a160-43d7-845b-66e9462a8bd2)), we assign the gravity direction to be opposite to the enclosed area (we assume that closed contours without slope lines are hills).
	- If all the contours have the gravity direction defined, we will skip Step 3.
- ## Step 3: Gravity Definition with Rain
	- We iterate over all the contours that already have a gravity defined, and for each of them we start a **Cold Rain Drop Production** (see [Rain Drop Production Definition](6aa1c9d2-3f4a-4b1e-8a6c-9e2d5f7b1a3c)). Every vote it casts is accumulated on the contour it was cast for, but no contour's gravity is assigned from those votes yet — a contour that receives one here stays undefined for the rest of this step, so a vote it receives from the Anti Rain Drop Production pass below can still be added to the same tally before either pass's own votes alone get to decide anything.
	- We iterate over the same contours that already had a gravity defined *before* the pass above ran — deliberately not the possibly-larger set that pass's own votes could go on to resolve, since none of them are resolved yet — and for each of them we start a **Cold Anti Rain Drop Production** — identical to a Cold Rain Drop Production, but every drop moves against gravity instead of along it. It accumulates its own votes the same way, onto the same per-contour tally the Rain Drop Production pass above already started.
	- Only once both passes above have finished do we define the gravity of each contour based on the combined votes it received across both. Contours with similar left and right votes should be reported as **warnings** from the algorithm. We define left and right votes as similar if the smaller of the two votes is more than **undefined_gravity_vote_threshold** of the other.
	- Restricting the Anti Rain Drop Production pass's own sources to contours already defined before the Rain Drop Production pass ran, rather than the larger set that pass's own votes go on to resolve, means a contour reachable only by a drop sourced from one of those newly-would-be-resolved contours' own geometry no longer gets one in this same run of Step 3 — this deliberately resolves fewer contours in one pass than sourcing from the larger set would, trading that reach for a vote tally that already reflects both directions before any side is chosen for a given contour. It is not only possible but expected, then, for some contours to still be undefined once both passes above have finished: a small enough **sources_per_contour_segment** (so a single drop's own path already reaches far enough to vote for every contour along it, rather than needing a second run seeded from a contour just resolved) is what closes that gap, not a further round of Step 3 itself.
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
	- For a human check of each step, I would like to see each step, one after the other, as an SVG file. The files are numbered in the order they're produced: `00_<map_name>_step1.svg`, `01_<map_name>_step1_close_search.svg`, `02_<map_name>_step1_growing_seeking.svg`, `03_<map_name>_step1_growing_matching.svg`, `04_<map_name>_step2.svg`, `05_<map_name>_step3_rain.svg`, `06_<map_name>_step3_anti_rain.svg`.
	- `<map_name>_contours_function.svg`: not one of the numbered per-step files above, and not a map at all -- a plain 2D plot of the contour-pixel potential-well force itself ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)'s **contour_force_magnitude**), `x` the distance from a single contour pixel in meters and `y` the resulting force, sampled every 0.1m from `0` to `2 * contour_force_second_equilibrium`. Drawn maroon, the same color as `02_<map_name>_step1_growing_seeking.svg`/`03_<map_name>_step1_growing_matching.svg`'s own contour-pixel push/pull vector layer, over a gray `y = 0` line and a gray `x = 0` line, so **contour_force_equilibrium**'s repulsion/attraction crossover and **contour_force_second_equilibrium**'s attraction plateau both read at a glance. Depends only on `--config`, not on the map being processed, so it is written once per `--create_svg` run before Step 1 even starts.
	- Every SVG below that draws the Contour Raster grid draws, under everything else, a single black polygon for the whole out-of-bound (`1`) area, some gray lines showing the pixel grid, plus a square for every pixel that is a contour (brown, **CONTOUR_0_MATRIX_VALUE** or above, the same brown regardless of which contour) or high density (light green) — `0` (undefined) and `2` (no contour, in bound) get no square of their own, since together with `1` they typically cover most of a real map's own raster and squaring every one of them made these files huge. Out of bound gets a shape rather than nothing, since seeing where it actually reaches matters for judging **growing_oob_seeking_max_steps**, but not a square per pixel either, for the same file-size reason: traced instead into a handful of closed rings (the raster's own bounding rectangle, plus one ring per out-of-bound/in-bound transition found) and drawn as one path with an even-odd fill rule, so the true shape (holes and all) comes through in space proportional to its own perimeter, not its area. The full, every-pixel-colored picture (light grey for `0`, black for `1`, white for `2`, light green for `3`, brown for a contour) instead goes into a companion `<map_name>_raster.png`, written once alongside `03_<map_name>_step1_growing_matching.svg` (the Contour Raster never changes again past that point -- it can still change between `02` and `03`, since Matching is still to come).
	- `00_<map_name>_step1.svg`, written once Step 1's Contour Raster (including the Jump high-density stamping and the out-of-bound computation) is complete, but before Step 1's own Growing Process sub-step runs: draws, on top of the palette above, two lines for each contour: one including the Bezier segments, in red, and one after linearization with only straight-line segments, in green. It should also print a blue arrow for each **LineGravityDefiners**, in the direction of the gravity it defines, starting from its definition location, and print several yellow arrows for each **LineGravityDefiners**, spanning the whole polygon it occupies. Draw each **LineGravityDefiners**' own buffered polygon filled, lightly, so the actual search area a **heavy_object_width**/**heavy_object_growing** choice produces can be judged by eye against the real pixels; draw every Heavy Object's own buffered polygon the same way, in a different fill color so the two are easy to tell apart. Additionally, draw a red ring (stroke only, no fill) centered on every Flying End's position — every open contour's start/end node not yet resolved to an out-of-bound or high-density pixel, before the Growing Process runs.
	- `01_<map_name>_step1_close_search.svg`: identical to `00_<map_name>_step1.svg`, including the same red rings still at their original, pre-growing positions, except written once the Growing Process's own preliminary Close Search pass has run: the Contour Raster and every contour's `ls` reflect whatever Close Search resolved (a merge, a close, or a landing directly onto an out-of-bound pixel), and any contour it touched is drawn in blue instead of green, the same rule `02_<map_name>_step1_growing_seeking.svg`/`03_<map_name>_step1_growing_matching.svg` (below) use. Unlike those two, this file draws no push/pull vectors and no integration-step dots -- Close Search is a one-shot geometric search, not a physics pass, so it has none to draw. On its own topmost layer, above everything else, it instead draws every search cone Close Search actually used, one per turn it gave a Flying End (whether or not that turn found anything): the cone itself, apex at the Flying End's own position, opening along its own forward direction, **searching_fov** degrees wide, out to **searching_distance** meters -- stroke only, unfilled, in purple, so the actual search area a **searching_fov**/**searching_distance** choice produces can be judged by eye against the real Flying Ends and pixels.
	- `02_<map_name>_step1_growing_seeking.svg` and `03_<map_name>_step1_growing_matching.svg`: each identical to `00_<map_name>_step1.svg` — including the same red rings, still shown at their original, pre-growing positions, kept for reference — except written after one of the Growing Process's own two phases has run (Seeking for the first file, Matching for the second), so the Contour Raster and every contour's `ls` reflect however far growing has gotten by that point, and the post-linearization line (the green one above) of any contour touched by growing so far, in either phase — grown, merged into another, or both — is drawn in blue instead. The red, pre-linearization layer is unaffected by a merge: it is always one subpath per originally-digitized contour object, drawn from that object's own raw node sequence alone, never spliced across a merge into a second, geometrically unrelated object's own raw trace (there is no meaningful single curve through two originally separate digitized objects) — so either file can show fewer distinct contours in blue/green than it does in red, once any contours have merged. Additionally, for every integration step *that file's own phase* took ([Appendix 5](6aa1e4f7-8b2d-4c6a-9f1e-2d8b4a6c9f3e)) — never the other phase's, each file's own diagnostic layer covering only its own phase's own steps — draw the four force contributions computed there as four separate vectors, tail on the Flying End's own position *before* that step, head at tail plus that contribution scaled by **growing_visualization_push_pull_vectors_scale** — so a vector's own length shows how strongly it pushed or pulled, not just its direction. One color per term: maroon for the contour-pixel potential well (**contour_force_max_repulsion**/**_equilibrium**/**_max_attraction**/**_second_equilibrium**) and magenta for **out_of_bound_force**, both dropped entirely during **Matching** and so always zero-length on `03_<map_name>_step1_growing_matching.svg`; dark green for **density_region_force** and deep sky blue for **flying_end_force**, both dropped entirely during **Seeking** and so always zero-length on `02_<map_name>_step1_growing_seeking.svg`. Each of the four terms now belongs to exactly one phase, so each file's own push/pull-vector layer only ever shows two of the four colors: maroon/magenta on `02_<map_name>_step1_growing_seeking.svg`, dark green/deep sky blue on `03_<map_name>_step1_growing_matching.svg`. On top of all of this, that same phase's own integration steps' own resulting positions — an ordinary still-flying one, or a tunneling-safe landing — are each drawn as a small green dot, so a Flying End's actual physics path, one phase at a time, can be traced by eye.
	- `04_<map_name>_step2.svg`: like `03_<map_name>_step1_growing_matching.svg`, and in addition, for each contour with a defined gravity, write yellow arrows all along the contour showing the gravity direction.
	- `05_<map_name>_step3_rain.svg` and `06_<map_name>_step3_anti_rain.svg`: each doing the same as `04_<map_name>_step2.svg`, one also showing the rain drops' paths as blue dots, the other also showing the anti rain drops' paths as red dots. Gravity is no longer assigned right after either pass alone — only once both have finished, from their combined votes (see Step 3 above) — so the rain file's own gravity-arrow layer is limited to whichever contours were already resolved *before* Rain Drop Production ran at all (Step 2's own resolutions), never one either rain-drop pass itself goes on to resolve, or a contour no drop from that set of sources ever reached would wrongly show a gravity arrow there anyway. The anti rain file, written once both passes are done and votes are tallied, shows the full final state. Under every drop's own dots, first trace its whole trail (source to evaporation) as a thin gray line, a third of the width used below for a vote segment, so the path itself reads as a line rather than a scatter of unconnected dots. Over that, under any dot still within that drop's **rain_drop_starting_voting_hysteresis** window, draw a black dot with a slightly larger radius, so it still peeks out from underneath. A vote belongs to the step it happened on, not to either endpoint alone, so draw it as a purple line segment from the drop's previous position to its current position at the moment it cast the vote, rather than a dot at a single point.
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
	- Given a Flying End's current position (see Step 1's Growing Process, whose merge check and matching-free/matching-restored gating this does *not* cover -- only the physics of a single integration step), this computes the net force acting on it and the resulting raw next position, before the caller's own tunneling-safe walk decides whether that lands it on a border/density pixel early.
	- The contour-pixel potential well: for every real contour or **TEMPORARY_CONTOUR** pixel within **contour_force_window** of the Flying End (excluding its own pixel and its 8 immediate neighbors), at Euclidean distance `x`, the magnitude `cfmr`/`cfe`/`cfma`/`cfse` (this doc's own shorthand for **contour_force_max_repulsion**/**contour_force_equilibrium**/**contour_force_max_attraction**/**contour_force_second_equilibrium**) is a cubic curve from `cfmr` at `x = 0` down to `0` at `x = cfe`, then a smoothstep from `0` up to `cfma` (negative) between `cfe` and `cfse`, then held constant at `cfma` beyond `cfse`. The magnitude alone carries the sign -- positive repels (pushes away from the pixel), negative attracts (pulls toward it) -- so every contribution is applied along the same "away from the pixel" unit vector, with no separate sign branch needed.
	- The three constant-magnitude-*window* forces: for every out-of-bound/high-density pixel, or other pending Flying End, within **attraction_force_window**, **out_of_bound_force**/**density_region_force**/**flying_end_force** (whichever applies, and phase permitting -- see Step 1's Growing Process) contributes along the unit vector toward it. Summed over every qualifying pixel/end found, not just the nearest. **out_of_bound_force**/**density_region_force** contribute that same fixed magnitude regardless of distance within the window; **flying_end_force** instead falls off (cubically in the normalized distance), from its own full magnitude at zero distance to zero at **attraction_force_window** itself -- see its own entry above for why.
	- All four terms sum into one net force; the raw next position is the Flying End's current position displaced by that net force times **grow_time_step** (the net force, in Newtons, used directly as meters/second -- no mass, no inertia).
	  ```rust
	  use geo::Coord;
	  
	  enum WindowPixelKind {
	    OutOfBound,
	    HighDensity,
	    Contour,
	  }
	  
	  /// One pixel (or other Flying End) found within reach, already
	  /// classified, with its world-space center.
	  struct WindowHit {
	    kind: WindowPixelKind,
	    center: Coord<f64>,
	  }
	  
	  /// The contour-pixel potential well's own scalar magnitude at Euclidean
	  /// distance `x` from a single contour pixel: positive (repulsive) up to
	  /// `cfe`, negative (attractive) from `cfe` to `cfse`, held constant at
	  /// `cfma` beyond `cfse`.
	  fn contour_force_magnitude(x: f64, cfmr: f64, cfe: f64, cfma: f64, cfse: f64) -> f64 {
	    if x <= cfe {
	      (2.0 * cfmr / cfe.powi(3)) * x.powi(3) - (3.0 * cfmr / cfe.powi(2)) * x.powi(2) + cfmr
	    } else if x <= cfse {
	      let t = (x - cfe) / (cfse - cfe);
	      cfma * (3.0 * t * t - 2.0 * t * t * t)
	    } else {
	      cfma
	    }
	  }
	  
	  /// The Flying End's net force this integration step: the contour
	  /// potential well summed over every `Contour` hit, plus the three
	  /// constant-magnitude forces summed over every `OutOfBound`/
	  /// `HighDensity`/other-Flying-End hit -- each of the four now belongs
	  /// to exactly one phase (folded here into `window_hits` as an ordinary
	  /// `WindowPixelKind` hit for simplicity, though `step1_extract`'s own
	  /// implementation scans other Flying Ends separately from the raster):
	  /// `seeking` keeps the contour potential well and `out_of_bound_force`,
	  /// dropping `density_region_force`/`flying_end_force`; `!seeking` --
	  /// i.e. Matching -- keeps `density_region_force`/`flying_end_force`,
	  /// dropping the contour potential well and `out_of_bound_force`.
	  #[allow(clippy::too_many_arguments)]
	  fn growing_net_force(
	    flying_end: Coord<f64>,
	    window_hits: &[WindowHit],
	    seeking: bool,
	    cfmr: f64,
	    cfe: f64,
	    cfma: f64,
	    cfse: f64,
	    out_of_bound_force: f64,
	    density_region_force: f64,
	    flying_end_force: f64,
	  ) -> Coord<f64> {
	    let mut force = Coord { x: 0.0, y: 0.0 };
	    for hit in window_hits {
	      let dropped = if seeking {
	        hit.kind == WindowPixelKind::HighDensity
	      } else {
	        matches!(hit.kind, WindowPixelKind::OutOfBound | WindowPixelKind::Contour)
	      };
	      if dropped {
	        continue;
	      }
	      let (dx, dy) = (hit.center.x - flying_end.x, hit.center.y - flying_end.y);
	      let d = (dx * dx + dy * dy).sqrt();
	      if d < 1e-12 {
	        continue; // degenerate: the pixel sits exactly on the Flying End
	      }
	      // Toward the pixel -- the right direction for `OutOfBound`/
	      // `HighDensity` (an attraction, applied as-is below), but the
	      // *opposite* of what `Contour`'s own magnitude expects (an "away
	      // from the pixel" unit vector, so its own contribution is applied
	      // negated, just below).
	      let (ux, uy) = (dx / d, dy / d);
	      match hit.kind {
	        WindowPixelKind::OutOfBound => {
	          force.x += out_of_bound_force * ux;
	          force.y += out_of_bound_force * uy;
	        }
	        WindowPixelKind::HighDensity => {
	          force.x += density_region_force * ux;
	          force.y += density_region_force * uy;
	        }
	        WindowPixelKind::Contour => {
	          let magnitude = contour_force_magnitude(d, cfmr, cfe, cfma, cfse);
	          force.x -= magnitude * ux;
	          force.y -= magnitude * uy;
	        }
	      }
	    }
	    force
	  }
	  
	  /// The raw next position: `flying_end` displaced by `force` (Newtons,
	  /// used directly as meters/second) times `grow_time_step` -- a zero
	  /// force leaves the Flying End exactly where it is. (Not shown here:
	  /// during Matching, the real implementation first scales a nonzero but
	  /// sub-`matching_min_force` `force` up to that magnitude, direction
	  /// kept -- see `matching_min_force`'s own entry above.)
	  fn raw_next_position(flying_end: Coord<f64>, force: Coord<f64>, grow_time_step: f64) -> Coord<f64> {
	    Coord {
	      x: flying_end.x + force.x * grow_time_step,
	      y: flying_end.y + force.y * grow_time_step,
	    }
	  }
	  ```
