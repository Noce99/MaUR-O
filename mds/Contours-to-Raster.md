- This is an algorithm description that should, given some contours and other terrain objects from a .omap, return a raster of the given map with, for each pixel, the elevation value up to a constant with respect to the real ones.
- ## Assumptions
  collapsed:: true
	- 1) This algorithm assumes that the gravity of the same contour line is always going in the same direction. In other words, it is not possible to find a contour line with slope lines pointing toward different parts of the same contour line.
	- The form lines are completely ignored
- ## Parameters
	- **bezier_linearization_step** (meters) — maximum chord length used to flatten a cubic Bezier segment into straight lines ([Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)). Should be noticeably smaller than **contours_step**: it runs first, and the curve detail it captures is exactly what the equal-chord resampling with **contours_step** could otherwise erase from a curve if the two were comparable.
	- **contours_step** (meters) — the equally-spaced node distance every contour's final `ls` is resampled to ([Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8)). This is the working resolution the rest of the algorithm (source placement, circle fitting) operates on.
	- **rasterization_px_size** (meters) — pixel size of the Contour Raster (Step 0). The finest positional resolution the whole algorithm can resolve; it must be small enough that no two genuinely distinct contours ever land in the same pixel — Step 0's crash-on-conflict check enforces this at runtime, so in practice it is bounded by the tightest contour bunching expected on the map (e.g. at cliffs).
	- **rasterization_step_factor** (pure number, multiplies **rasterization_px_size**) — how finely a contour's `ls` is re-densified before writing it to the Contour Raster ([Appendix 3](6aa13658-fddb-4da2-87b5-27df9d8f2df3)). The write itself no longer needs this to be ≤ 1 now that it walks pixels with the [Appendix 5](6aa157fd-862a-4e2d-9b2c-2184a96494cc) supercover trick, but a coarser value still blurs how closely the densified points track the actual curve.
	- **heavy_object_width** (meters, `width` in [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e)) — buffer width used to turn a **Jump**'s or a **Heavy Object**'s own `ls` into a polygon. For a Jump, this is the area Step 1 scans to find which contours it touches; for a Heavy Object, it is the area Step 0 itself searches for an intersecting contour, instead of only the pixels directly under its digitized line. Should be at least a couple of **rasterization_px_size**, so the buffered polygon covers a real ring of pixels rather than collapsing into a single pixel row.
	- **heavy_object_growing** (meters, `extra_growing` in [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e)) — extra padding buffered on top of **heavy_object_width**. Normally much smaller than **heavy_object_width** itself — a small safety margin, not a second width.
	- **circumference_fitting_points_number** (pure number) — how many **Coord**s on each side of a Heavy Object/contour intersection are used to fit a circle (Step 0). Should be a small multiple of the point density set by **contours_step**: enough points for a stable fit (at least 3), but few enough that the sampled arc still reflects local curvature at the intersection rather than the contour's shape further away.
	- **slope_lines_contours_search_radius** (meters) — how far around a Slope Line's own position Step 0 searches for the nearest Contour Raster pixel to attribute its gravity reading to. A Slope Line's placement on the map is not always pixel-exact on top of its own contour, so a plain under-the-point lookup misses legitimate readings; too small and this still happens, too large and a reading risks being attributed to the wrong, merely-nearby contour instead.
	- **rain_drop_step** (meters) — distance a rain drop advances per simulation step (Step 2). Should stay small relative to **rasterization_px_size** (a few pixels at most); [Appendix 5](6aa157fd-862a-4e2d-9b2c-2184a96494cc)'s supercover check still finds every pixel a step crosses even for a large step, but a coarse step blurs exactly where along the path a vote was triggered and raises the chance of overshooting the map boundary or the source's own contour.
	- **sources_per_contour_segment** (pure number) — how many sources are placed per contour segment (Step 2). Should scale with how long segments are relative to **rain_drop_step**: the larger **contours_step** is, the higher **sources_per_contour_segment** needs to be to keep sources close enough together to resolve nearby thin contours individually.
	- **rain_drop_starting_voting_hysteresis** (pure number, counted in **rain_drop_step**-sized steps) — a rain drop is exempt from evaporating on re-crossing its own starting contour within this many steps of being created, and, separately, exempt from evaporating on re-crossing any other contour it has already voted for within this many steps of *that vote* (Step 2). Should be just large enough (a handful of steps) to carry the drop clear of either; since each exemption is measured from its own reference point (creation, or the vote itself), it does not need to be sized relative to the distance to neighboring contours or to how far into the drop's path a vote happens to fall.
	- **undefined_gravity_vote_threshold** (pure number, a ratio in (0, 1)) — how close left and right vote counts must be before being flagged as ambiguous (Step 2). A value near 1 only flags near-exact ties; a value near 0 flags votes on both sides however lopsided.
	- **contour_gap_merge_radius** (meters) — how close a Contour Raster pixel conflict must be to *both* contours' own start or end node for Step 0 to join them into one contour instead of crashing (Step 0). Real contour digitizing sometimes splits one physical line into two objects whose endpoints are close but not exactly coincident; too small and a genuine gap like that still crashes, too large and two contours that only happen to end near each other risk being wrongly joined.
- ## Step 0: Extrapolate Elevation Information from an .omap
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
	- **Gravity direction semantics**: `gravity_dx`/`gravity_dy` are only ever *fixed* relative to the first segment (`ls[0] -> ls[1]`) — they are not meant to be compared directly, as a raw vector, against a gravity reading taken somewhere else along a curved contour. What they really encode is a **side**: since a contour's gravity direction is the same everywhere along it (see Assumption 1), fixing the perpendicular direction at `ls[0]` is equivalent to picking one of the two sides of the LineString (left or right of its parameterization direction) as "downhill" for the whole contour. To get the actual gravity vector at any *other* point of the contour — e.g. to compare against a `PointGravityDefiners` reading in Step 1, or to know which way a rain drop should leave a source in Step 2 — compute the local tangent at that point from its neighboring **Coord**s, take its two perpendicular unit vectors, and pick the one on the same side — left/right, via the sign of the 2D cross product against the tangent — that `(gravity_dx, gravity_dy)` is on relative to the `ls[0] -> ls[1]` tangent:
	  ```rust
	  use geo::Coord;
	  
	  /// Which side of the tangent `tangent_from -> tangent_to` the vector
	  /// `(dx, dy)` points to: a positive result means left, a negative
	  /// result means right. Comparing the sign computed here at
	  /// `ls[0] -> ls[1]` against the sign computed at any other point's
	  /// local tangent is how "in accordance" is decided everywhere in
	  /// Step 1 and Step 2 — never a direct comparison of raw (dx, dy)
	  /// components, which are only valid relative to the tangent they
	  /// were computed against.
	  fn side_of_tangent(tangent_from: Coord<f64>, tangent_to: Coord<f64>, dx: f64, dy: f64) -> f64 {
	      let (tx, ty) = (tangent_to.x - tangent_from.x, tangent_to.y - tangent_from.y);
	      tx * dy - ty * dx
	  }
	  ```
	- We instantiate a 2D **Vector** of **u32** (initially full of zeros) that is a rasterization of the map area itself, with a pixel size in meters defined by the **rasterization_px_size** parameter. The 2D **Vector** should be filled with all the contours' values in the following way: for each contour we densify its **ls** following [Appendix 3](6aa13658-fddb-4da2-87b5-27df9d8f2df3) and then, for each consecutive pair of **Coord**s of the densified **ls**, we walk every pixel that the segment between them touches using the same supercover traversal described in [Appendix 5](6aa157fd-862a-4e2d-9b2c-2184a96494cc) (the source-contour exclusion from that appendix does not apply here — every touched pixel is written the same way). Densifying alone is not enough to avoid gaps: two consecutive densified points can land in diagonally-adjacent pixels, leaving the shared corner pixel unwritten, which is exactly the gap a supercover walk closes. For each pixel touched we write the index of the contour +1 (where by "index" we mean the index inside the previously defined **Vector** of **Contours**; the plus one is because 0 already means "no contour"). Before writing it we should check whether that pixel already holds a different index value. If it is the same value we are writing, it is simply a no-op. If it is a different index, before crashing we should first check whether every single conflicting pixel this contour produces against that other contour (not only the one found first) falls within **contour_gap_merge_radius** of *both* contours' own start or end node -- real contour digitizing sometimes splits one physical line into two objects whose endpoints are close but not exactly coincident, and this is exactly what that split looks like on the raster: a small cluster of conflicting pixels right at the gap, rather than a long run of them along the whole length. If that is the case, join the two contours into one (their nodes concatenated, oriented so the two close ends meet, with no attempt to also merge their still-separate raw, pre-rasterization node sequences kept only for visualization) and continue instead of crashing. Otherwise -- any conflicting pixel far from either contour's own start or end, which is what a long run of genuinely parallel, too-closely-spaced contours produces -- the program should crash, suggesting to decrease the **rasterization_px_size**.
	- All the other symbols (**Slope Lines**, **Jumps** and **Heavy Objects**) are just used to understand the gravity direction and are stored inside a **Vector** of instances of **LineGravityDefiners** and a **Vector** of **PointGravityDefiners** in the following way:
		- The **Slope Lines** are point symbols whose gravity direction can be read directly from the .omap file. For each one, we search the Contour Raster for the nearest non-zero pixel within **slope_lines_contours_search_radius** ground meters of its position (a Slope Line's placement on the map is not always pixel-exact on top of its own contour): if none is found, we emit a warning and skip it; otherwise we record that pixel's reference contour (raster index − 1) and append it to the **Vector** of **PointGravityDefiners.**
		  ```rust
		  struct PointGravityDefiners{
		  	x: f64,
		      y: f64,
		      reference_contour: u64,
		      gravity_dx: Option<f64>,
		      gravity_dy: Option<f64>,
		  }
		  ```
		- The **Jumps** are line symbols that should first of all be transformed into a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** following [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8). After that they should be transformed into polygons following [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e). After that we append them to the **Vector** of **LineGravityDefiners**, a struct defined below, where the **ls** of `LineWithGravity` should be the **LineString** resulting from Appendix 1 and the **gravity_direction** should be retrieved from the .omap object.
		  ```rust
		  struct LineGravityDefiners{
		      lwg: LineWithGravity,
		      poly: Polygon<f64>,
		  }
		  ```
		- The **Heavy Objects** are line symbols, so they should also first of all be transformed into a **[LineString](https://docs.rs/geo/0.33.1/geo/geometry/struct.LineString.html)** following [Appendix 1](6aa103ad-0d81-46fc-bc7e-5d07c29b4be8), then into a buffered polygon following [Appendix 2](6aa11a72-e5ae-4ca3-ab52-379efa00d61e) (using the same **heavy_object_width**/**heavy_object_growing** parameters a Jump's own polygon uses). The difference with respect to **Jumps** is that their gravity direction cannot be obtained from the .omap file directly, but needs to be inferred by considering the shape of the contours that intersect with them and the fact that they are not lines always perpendicular to gravity, but always parallel to it. We can still get information from them in the following way. We rasterize the buffered polygon (the same way a Jump's own polygon is, in Step 1) and, for each of its pixels with a non-zero index in the 2D Contour Vector, we have a candidate intersection with that contour (remember to subtract 1 from the index). Searching the whole buffered area, not just the pixels directly under the digitized line, catches a contour that runs close to a Heavy Object without landing exactly on it — the same motivation as **slope_lines_contours_search_radius** for Slope Lines. Since the polygon commonly covers several pixels of the very same nearby contour, we group its matched pixels by contour index and treat each distinct contour as a single intersection, at the centroid of that contour's own matched pixels within the polygon, rather than one intersection per pixel. For each such intersection we should compute the circumference that fits the contour in the section around it (the number of **Coord**s to use before and after the intersection for the fit is defined by a parameter, **circumference_fitting_points_number**), and we should define the gravity of that intersection based on the vector starting from the intersection and ending at the center of the fitted circumference. Now we have a point, we know the reference contour, and we also know the gravity direction, so we can append an item to the **Vector** of **PointGravityDefiners**.
- ## Step 1: Obvious Gravity Definition
  collapsed:: true
	- We scan all the items in the **Vector** of **PointGravityDefiners** and, for each one, we set the gravity direction of its reference contour if not already defined, or check whether it is in accordance with the already-set gravity otherwise. The algorithm should crash with an error explaining the problem if they do not match.
	- We scan all the items in the **Vector** of instances of **LineGravityDefiners** and we rasterize the corresponding **poly** (using the same parameters used for the Contour Raster creation), then check, for each pixel of the poly, whether we have a corresponding non-zero index in the Contour Raster. If the index is non-zero, we set the gravity of that contour to the gravity of the **LineGravityDefiners** if it is not already defined, or check whether it is in accordance with the already-set contour gravity otherwise. The algorithm should crash with an error explaining the problem if they do not match.
	- We scan all the closed contours (we should check whether the LineString is closed) without a defined gravity direction. If they do not contain any other contours (see [Appendix 4](6aa14c07-a160-43d7-845b-66e9462a8bd2)), we assign the gravity direction to be opposite to the enclosed area (we assume that closed contours without slope lines are hills).
	- If all the contours have the gravity direction defined, we will skip Step 2.
- ## Step 2: Gravity Definition with Rain
	- We iterate over all the contours that already have a gravity defined, and for each of them we start the **Rain Drop Production**:
		- The parameters of the *Rain Drop Production* are 4: the **rain_drop_step** (meters), **sources_per_contour_segment** (pure number), **rain_drop_starting_voting_hysteresis** (pure number) and **undefined_gravity_vote_threshold** (pure number).
		- Given a contour, for each segment of its **LineString** we create **sources_per_contour_segment** **sources**, equally spaced. If **sources_per_contour_segment** is 3 we create a source at A, A + 0.33\*(B-A) and A + 0.66\*(B-A), where A and B are the starting and ending **Coord**s of the segment.
		- Each **source**'s own direction (perpendicular to the contour, in accordance with its gravity) is a blend of the direction *at* node A and the direction *at* node B, weighted by the same fraction used to place that source along the segment -- the source at A is 100% A's own direction, the one at A + 0.33\*(B-A) is 66% A's direction and 33% B's, and so on, rather than every source along a segment leaving in the same, flat direction. A node's own direction is the mean of the perpendicular of the segment before it and the one after (a node with only one neighboring segment -- an open contour's first or last node -- takes that one alone; a closed contour's first node wraps its "previous" segment around to the contour's own last one). The direction of each **rain drop** never changes once it starts.
		- We let each **rain drop** step **rain_drop_step** meters in its direction, and we continue until the rain drop either leaves the map or encounters another contour (or the same one). To check whether the rain drop has hit a contour, see [Appendix 5](6aa157fd-862a-4e2d-9b2c-2184a96494cc).
		- If the **rain drop** leaves the map, it disappears (*evaporates*).
		- If the **rain drop** encounters a contour whose gravity is already defined, it disappears (**evaporates**). In the first **rain_drop_starting_voting_hysteresis** steps since it was created, do not let the rain drop evaporate if it crosses its starting contour — evaporating on the starting contour only becomes possible once that many steps have elapsed.
		- If the **rain drop** encounters a contour with an undefined gravity, we add a vote for the corresponding gravity side and let the drop continue its travel (to prevent the same rain drop from voting twice for the same contour, each rain drop should remember the list of contour indexes it has already voted for, together with the step at which it voted for each). If it encounters one of them again, the same **rain_drop_starting_voting_hysteresis** exemption applies as for the starting contour above, but measured from *that vote's own step* rather than from the drop's creation, and shifted one step later to compensate for the step the vote itself was cast on (which is not a free re-crossing chance the way the drop's own creation step is): within **rain_drop_starting_voting_hysteresis** steps *after* the vote it does not evaporate either, and simply continues without voting again; past that many steps, encountering it again evaporates the drop. Measuring from the vote itself (not from creation) matters because a vote can happen arbitrarily late in a drop's path, far past its own starting-contour window.
		- When all *rain drops* have evaporated, we finish the **Rain Drop Production** phase by defining the gravity of each contour based on the obtained votes. Contours with similar left and right votes should be reported as **warnings** from the algorithm. We define left and right votes as similar if the smaller of the two votes is more than **undefined_gravity_vote_threshold** of the other.
	- If all the contours have the gravity direction defined, the step is finished.
	- We iterate over all the contours that already have a gravity defined, and for each of them we start the **Anti Rain Drop Production**:
		- It's basically the same as the **Rain Drop Production**, but now we produce *anti-rain drops* that behave exactly like *rain drops*, except that they travel against gravity.
	- At this point it should be impossible to have contours with undefined gravity. Actually is possible with a non small enough **sources_per_contour_segment**
- ## Step 3: Elevation Value Assignation
	- Assuming that each contour has the gravity direction defined (the algorithm should crash otherwise)
	- Let's describe a new process called **Hot Rain Drop Production** that target a contour called **C**. It's similar to the **Rain Drop Production** one with the only difference that when we have a rain drop evaporation because of hitting a contour (useless to check if it has a gravity direction defined because it must be the case considering the previous assumption) we set the **elevation_height** of the hitted contour. We set it considering the **accordance** or **discordance** of the rain drop direction and the contour gravity direction. In case of **accordance** we set the height as the **C** height minus 1. In case of **discordance** we set it as the same height value of **C**. If the hitted contour has already a height value check that it's equal to the one that we would have written otherwise crash with an error (This should not be possible to happen).
	- In a similar way we can define also an **Anti Hot Rain Drop Production** exactly equal to **Hot Rain Drop Production** but with anti-rain drop instead.
	- A random contour is selected and its **elevation_height** is set to 0.
	- From the random selected contour we execute a **Hot Rain Drop Production** and an **Anti Hot Rain Drop Production** keeping tracks (in both) of all the contours that we are setting the height off in a list that we will call **LHS**= List of Height Set (of course we do not add at the list the contours that has been hitted but already had a defined height).
	- When all the rain drops evaporates: for each element inside **LHS** we compute again a **Hot Rain Drop Production** +  **Anti Hot Rain Drop Production** phase and we continue recursively.
	- At this point, considering a small enough **sources_per_contour_segment** all contours should have a defined height. With a generic parameter value could be that some contours has still not a defined height. Anyway instead of crashing we can still try to recover the contours that still are missing an height (list of contours that we call **AC**=Alone Contours) as described in the following.
	- For each contour in **AC** we run a **Reverse Hot Rain Drop Production** + **Reverse Anti Hot Rain Drop Production** that are working exactly like their non *reverse* version but instead of setting the hitted contour height based on the source contour they set the height of the source based on the hitted contour height value.
	- If there are still contours without set height the algorithm should crash suggesting a smaller **sources_per_contour_segment**.
- ## Step 4: Final Tiff Computation
	- In this phase, given the height of each contour we have to write a new raster 2D vector of pixel size of **elevation_raster_pixel_size** in meters.
	-
- ## Visualization
	- For a human check of each step, I would like to see each step, one after the other, as an SVG file.
	- After Step 0, create an SVG file that draws a brown square for each pixel with a non-zero value in the Contour Raster 2D Vector (also some gray lines showing the pixel grid), and draws two lines for each contour: one including the Bezier segments, in red, and one after linearization with only straight-line segments, in green. It should also print a blue arrow for each **LineGravityDefiners**, in the direction of the gravity it defines, starting from its definition location, and print several yellow arrows for each **LineGravityDefiners**, spanning the whole polygon it occupies. Draw each **LineGravityDefiners**' own buffered polygon filled, lightly, so the actual search area a **heavy_object_width**/**heavy_object_growing** choice produces can be judged by eye against the real pixels; draw every Heavy Object's own buffered polygon the same way, in a different fill color so the two are easy to tell apart.
	- After Step 1, create an SVG file like the one before, and in addition, for each contour with a defined gravity, write yellow arrows all along the contour showing the gravity direction.
	- If Step 0 ends up crashing on an unresolved Contour Raster conflict (the two contours involved really are too close together, not just a small digitizing gap -- see the crash condition above), still write one more SVG, next to whichever of the four above already exist by that point: every other already-accepted contour in a much lighter color, for terrain context, plus the already-accepted contour actually involved and the newly read one that collided with it in their own distinct colors -- every one of them drawn only in a small local window around the conflict (since any can otherwise be most of the map), every conflicting pixel itself as a solid square, and a ring drawn around the whole conflicting cluster, so the two too-close contours can be inspected, against the shape of the terrain around them, without first decreasing **rasterization_px_size** and re-running.
	- After Step 2, create two SVG files, each doing the same as Step 1: one also showing the rain drops' paths as blue dots, the other also showing the anti rain drops' paths as red dots. Anti Rain Drop Production always runs right after Rain Drop Production and both write gravity into the same contours, so by the time either file is written every contour either pass ever resolved already has its final gravity -- the rain file's own gravity-arrow layer must still be limited to whichever contours were already resolved by the time Rain Drop Production itself finished (Step 1's own resolutions, plus Rain Drop Production's), not extended to ones only Anti Rain Drop Production goes on to resolve afterward, or a contour no rain drop ever reached would wrongly show a gravity arrow there anyway. The anti rain file, written once both passes are done, shows the full final state. Under every drop's own dots, first trace its whole trail (source to evaporation) as a thin gray line, a third of the width used below for a vote segment, so the path itself reads as a line rather than a scatter of unconnected dots. Over that, under any dot still within that drop's **rain_drop_starting_voting_hysteresis** window, draw a black dot with a slightly larger radius, so it still peeks out from underneath. A vote belongs to the step it happened on, not to either endpoint alone, so draw it as a purple line segment from the drop's previous position to its current position at the moment it cast the vote, rather than a dot at a single point.
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
  id:: 6aa13658-fddb-4da2-87b5-27df9d8f2df3
  collapsed:: true
	- Given a **LineString** we can densify it using the **rasterization_step_factor** and **rasterization_px_size** parameters, with the code below.
	  ```rust
	  use geo::algorithm::line_measures::Euclidean;
	  use geo::Densify;
	  let dense_ls = Euclidean.densify(&c.ls, rasterization_step_factor*rasterization_px_size);
	  ```
- ## Appendix 4
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
- ## Appendix 5
  id:: 6aa157fd-862a-4e2d-9b2c-2184a96494cc
  collapsed:: true
	- Given the previous and current position of a **rain drop** (a step of length **rain_drop_step**, typically several pixels long), we need to check every pixel of the **Contour Raster** that the segment between them actually touches, not just the pixel at either endpoint — otherwise a thin (single-pixel-wide) rasterized contour can be stepped over without being detected. Converting `prev`/`next` to their own pixel indices first and walking a supercover line traversal between *those two cells* (e.g. the [line_drawing](https://crates.io/crates/line_drawing) crate's `Supercover` iterator) is not enough: it only ever sees which cell each endpoint floors into, never where within that cell it actually sits, nor the continuous line's real path between them — a thin, diagonally-placed contour can clip a multi-pixel-long step for a fraction of its length without either endpoint's own pixel being anywhere near it, and that crossing is missed entirely. Instead we walk the continuous segment directly in pixel space: a standard grid-traversal/DDA algorithm (Amanatides–Woo style) that tracks the exact parametric position at which the segment crosses each vertical or horizontal pixel boundary, so every cell the line geometrically touches is found regardless of how long the step is or where exactly within their own pixels the endpoints fall. A step that passes exactly through a shared corner of four pixels visits both cells flanking that corner (not just the two diagonal ones), the same conservative convention a supercover traversal itself uses for a corner crossing.
	- The rain drop's own **source contour** must be excluded from this check (its index is carried alongside the drop for its whole lifetime), otherwise the drop would evaporate against its own origin on its very first step. Excluding it here, at the traversal level, is where that exception belongs rather than downstream in the Rain Drop Production logic.
	- The function returns the first contour index it finds along the step (other than the excluded source), or **None** if the step is clear. What the caller does with that index (evaporate on a defined-gravity contour, vote and continue on an undefined one) is decided in Step 2, not here — this appendix only answers "did this step cross a contour, and which one."
	- ```rust
	  use geo::Coord;
	  
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
	  /// Raster and returns the first contour index encountered (excluding
	  /// `exclude_contour_idx`, the rain drop's own source), or None if the
	  /// step is clear.
	  fn step_traversal_hit(
	    prev: Coord<f64>,
	    next: Coord<f64>,
	    origin: Coord<f64>,
	    rasterization_px_size: f64,
	    contour_raster: &Vec<Vec<u32>>,
	    exclude_contour_idx: u64,
	  ) -> Option<u64> {
	    let mut hit = None;
	    walk_pixels(prev, next, origin, rasterization_px_size, |x, y| {
	      if x < 0 || y < 0 { return true; }
	      let (x, y) = (x as usize, y as usize);
	      let Some(row) = contour_raster.get(y) else { return true };
	      let Some(&val) = row.get(x) else { return true };
	  
	      if val == 0 {
	        return true; // empty pixel, keep walking
	      }
	      let contour_idx = (val - 1) as u64; // undo the "+1" offset from Step 0
	      if contour_idx == exclude_contour_idx {
	        return true;
	      }
	      hit = Some(contour_idx);
	      false // found one, stop
	    });
	    hit
	  }
	  ```