# MOT — MaUR-O Tiles (draft, version 1 design)

Status: **design draft**. Nothing here is implemented yet; the measurements in
[Measurements so far](#measurements-so-far) come from the throw-away programs in
`examples/` (`tile_stats.rs`, `curve_fit.rs`, `hidden_areas.rs`).

MOT is a compact, tiled, vector format for storing *all* orienteering maps of the
world as one map, and for fetching "what is around this location" cheaply. It is
the storage counterpart of the `.omap` format: an `.omap` file is plain XML that
carries the symbol set *and* the objects of a single map; MOT carries only
objects, and shares one symbol set per layer across the whole world.

## Goals

1. **Fast retrieval by location.** A viewer asks for a bounding box (or a tile)
   and gets every object near it without reading anything else.
2. **Small.** Both on disk and over the wire. The target is well below what a
   gzipped `.omap` reaches (see measurements).
3. **One world map.** Not many separate maps: a single map per symbol set, in
   which individual surveys are only *coverage* metadata.
4. **Versioned.** The history of every object is kept, so a viewer can step back
   in time and see who changed what.
5. **Stay a library.** The crate defines the format, the codec and the queries.
   Logins, servers, concurrent writes, permissions and UI belong to o-mnia (see
   [Responsibilities](#responsibilities)).

Non-goals for version 1: editing in place, simplifying geometry (see
[Future adaptations](#future-adaptations)), templates/images, anything that
only matters to Mapper (print settings, part tabs, view state).

## Layers: one world map per symbol set

A **layer** is one world map drawn with one symbol set (ISOM, ISSprOM, ISSkiOM,
ISMTBOM, indoor, ...). A layer has:

- its own symbol set, defined once in the format specification / a versioned
  table (id → kind, colour, draw order, name). Objects store only the symbol id;
- its own tile pyramid;
- its own **coverage** (see below).

The same ground mapped for several disciplines (foot, ski, MTB) is simply the
same place in several layers; they never conflict.

A viewer can:

- show one layer;
- show *all* layers at once by giving them a **priority order** (for example
  ISOM, then ISSprOM, then ISSkiOM...): at every point it draws the highest
  priority layer that covers that point, and masks the layers below it inside
  that coverage. Without masking, a tile with an ISOM map in one corner and an
  ISSkiOM map in the rest would draw both symbol sets on top of each other.

### Coverage

Each layer stores **coverage polygons** (the area actually surveyed), separate
from the objects. They are needed for two reasons:

1. *masking* in the stacked view (above);
2. telling **mapped but empty** (a stretch of plain forest has no objects) from
   **never mapped**.

Coverage replaces the "map table" idea: there are no separate maps as visible
entities, only footprints with provenance (source, date, licence).

### Scale

Scale is **not stored in the data**. Geometry is in ground metres; a symbol's
size is paper millimetres × the *display* scale, and choosing it is the
viewer's job (a sprint map shown at 1:4000 gets one black-cross size, shown at
1:2000 another). The data may carry an optional *suggested display scale* per
coverage polygon, a hint and nothing more.

The converter from `.omap` multiplies paper millimetres by the map's own scale
to get ground coordinates.

## Tiles

- **Web Mercator quadtree**, `z/x/y`, as in slippy maps. Tiles at zoom 13–14 are
  a few km wide, about the size of an orienteering map. (A fixed 1°×1° grid was
  rejected: 1° is ~111 km, a tile would hold dozens of maps and the viewer would
  download far more than it shows.)
- Empty tiles cost nothing: the "islands in the ocean" case is free.
- Objects crossing a tile border are **clipped** to each tile they touch (with
  the flags below), the way vector tiles do it.
- Inside a tile, objects are **sorted by symbol draw order** and the symbol id is
  run-length coded, so a renderer walks a tile in layer order without sorting.
- Coarser zoom levels hold **generalised** tiles (small objects dropped, lines
  simplified) so the whole world can be shown at once.
- Each tile is compressed as a unit (zstd or brotli; brotli if the browser
  should decompress it natively).

### Container

One file, clustered by a Hilbert curve, with a directory giving O(1) lookup of
`(layer, z, x, y)` → byte range, so a browser can fetch single tiles with HTTP
range requests from a static host or CDN. This is PMTiles' design; either write
a small container of our own with the same ideas, or reuse PMTiles and keep only
the tile *payload* ours. Tiles are addressed by content hash and the directory is
versioned, so a CDN can cache them forever and a change invalidates only the
tiles it touches.

## Tile payload

### Coordinates

- Integers on a fixed grid (**1 cm**, or **5 cm**; a header parameter, see
  [Decisions](#decisions)), as **deltas** from the previous node, zigzag-coded, varint.
- Relative to the tile's origin, so the first node of an object is a delta from
  the previous object's first node (or the tile origin), not an absolute value.
- Separate streams for flags, dx and dy compress better than one interleaved
  stream.

### Objects

Per object: symbol id (run-length coded in the symbol-sorted order), optional
rotation (rotatable symbols only), geometry, and the extras a symbol kind needs:

| Kind | Geometry | Extras |
| --- | --- | --- |
| Point | one position | rotation |
| Line | path | — |
| Area | path with holes | pattern rotation and origin, only where the symbol has a pattern |
| Text | one or two anchors | text, horizontal/vertical alignment, box size |

### Paths

A path is a command stream, like MVT's, extended with cubic curves:

- `MoveTo`, `LineTo(n)`, `CurveTo(n)`, `Close`;
- a run of `LineTo` is stored as bare deltas;
- a cubic is two control-point deltas plus an end-point delta (control points
  stay near their node, so the deltas are small);
- Mapper's per-node flags are kept: dash point, gap, hole start, close;
- a flag marks an edge **created by clipping at a tile border**, so a renderer
  does not draw an area's outline along the tile edge;
- dashed lines keep a consistent dash phase across tiles (store the offset at the
  cut, or keep a small overlap buffer).

Curves are coded losslessly in version 1. Coarse zoom levels flatten them.

### Coordinate frame

Coordinates are stored in **Web Mercator** grid units, relative to
the tile origin. Ground resolution is then at least as fine as the grid size at every
latitude, at a small cost in bits at high latitudes. The converter has to
reproject: ground = paper × map scale, rotated by the grivation, projected from
the map's CRS to Mercator. Maps without a georeference cannot be placed and are
rejected.

## History

Every object has a **stable id** (the same in every tile it was clipped into) and
a **version**. A version is valid from one changeset to another; the current
version has an open end. Modification is append-only: moving a contour closes the
old version and creates a new one; undoing a change is a new, inverse changeset.
This is the model of OpenStreetMap's full-history data.

Each tile has two parts:

- **head**: only the current objects. This is what a viewer loads by default, and
  it is as small as a tile without history;
- **history** (sidecar): closed versions. Loaded only when the user uses the
  timeline.

The map at time *T* is head ∪ history, filtered on `from ≤ T < to`.

**Changesets** are a separate table: id, timestamp, author id, comment, tool.
Objects hold only the changeset id (one varint). The author id is opaque to the
format: identities and logins are o-mnia's. Optional periodic snapshots (yearly)
speed up time travel on long histories. The timeline of a viewer comes from the
changeset table (for example a histogram of changes in the visible area, with
author and comment).

## Responsibilities

Source of truth: the **changeset log plus the store of whole, unclipped
objects**. Tiles are a **derived artefact**, rebuilt for just the tiles a
changeset touches.

| In this crate (MaUR-O) | In o-mnia |
| --- | --- |
| The tile format (head and history) | Login and identities |
| Encode/decode: objects ↔ tiles, `Map` ↔ tiles | The server or database holding the log and the tiles |
| "Apply a changeset to a tile", "read this tile at time *T*" | Concurrent edits, permissions, moderation, revert |
| Bounding-box query | The UI: timeline, layer picker |
| CLI tools: `omap_to_tiles`, `tiles_query` | Calling the crate to rebuild tiles after a change |
| Rendering from decoded tiles (also in wasm) | |

If the crate also had to ship a store with its own writes (a file or SQLite), that
would be a much bigger project and would break the library-plus-CLI rule in
`CLAUDE.md`.

## Measurements so far

Taken with the programs in `examples/`, on a 1:4000 city map (Bologna /
Casalecchio, 75 MB `.omap`) and a 1:15000 forest map (Corno alle Scale, 23 MB).
"Raw" is the proposed delta/varint stream, one interleaved stream, objects sorted
by symbol, first node of each object absolute, one flag byte per node, 1 cm grid.

| | Bologna | Corno |
| --- | --- | --- |
| Objects / nodes | 271,051 / 2.68 M | 57,362 / 669 k |
| Curve starts | 7.7% of nodes | 13.4% of nodes |
| `.omap` | 74.7 MB | 22.7 MB |
| `.omap` + gzip -9 | 17.5 MB | 4.4 MB |
| MOT estimate, raw | 14.0 MB | 3.4 MB |
| MOT estimate, zstd -19 | 9.7 MB | 2.4 MB |
| MOT, 5 cm grid, zstd -19 | 8.3 MB | — |
| MOT, 10 cm grid, zstd -19 | 7.4 MB | — |

About 9× smaller than the XML and about 1.8× smaller than its gzip. The estimate
leaves out tile-local origins and separate streams (expected to save another 10–20%).
The bigger win is **per-tile access**: reading one neighbourhood without
decompressing the whole map.

## Future adaptations

These were measured or discussed and deliberately left out of version 1. The
format should not prevent any of them.

### Curve fitting

Replacing runs of near-collinear straight segments by cubic beziers. Tolerance
has to be in *paper* units (hand-digitised points are noisy, so 3 cm on the
ground fits almost nothing). With 0.03–0.05 mm of paper: Corno saves 6–9% after
zstd, Bologna saves nothing (control points cost more entropy than the nodes
they replace). Revisit if a better fit (with protected corners and dash points,
and shared-border awareness) shows a clear gain.

### Removing hidden overlap between areas

Only opaque solid fills hide what is under them. Measured: 4–7% of the fill
area is hidden, only 3 (Corno) and 562 (Bologna) objects are hidden entirely
(0.2% of the nodes), and the rest are partly hidden, where clipping adds nodes
along the cuts. Little to gain. If done, do it **only when building display
tiles**, never on the source objects: deleting the road above a meadow must make
the meadow reappear.

### Faces with shared borders (planar subdivision)

Instead of independent area objects, a plane of **faces** whose borders are
stored once and referenced by the faces on both sides (the TopoJSON idea). Empty
space is a face too. Measured: half of the area-border length is shared, so
area borders shrink to about 75% — about 2% of the total on Corno, 7% on
Bologna. Benefits beyond size: no gaps or slivers between neighbours, no draw-order
problems between solid areas, point-in-face queries.

Things the model would have to handle: a face carries a *set* of visible symbols,
not one (patterns over fills, with per-object pattern origin and rotation);
lines that run along a border (an edge may carry a line symbol); curves, which
must be flattened to compute the overlay; and editing and history, since moving
a shared border changes two areas and deleting an area loses what was under it.

It fits as a **derived display format built when tiles are built**. Version 1
reserves an object kind "face" and an "arc table" in the tile layout (empty in
version 1) so it can be added without breaking existing files. The 1 cm
integer grid makes the planar overlay exact (no floating-point robustness
issues).

### Other

- A better container (a purpose-built one instead of PMTiles, or PMTiles itself).
- Level-of-detail rules per symbol (which symbols survive which zoom).
- A "suggested display scale" per coverage polygon.
- Raster tiles as a fallback for viewers without a vector renderer.

## Decisions

Settled in discussion (recorded here so the codec can rely on them):

1. **Coordinate frame: Web Mercator**, relative to the tile origin. The grid is a
   **header parameter**: 1 cm is fine, and **5 cm is acceptable** (it saves ~15%;
   Mercator units are larger than ground units away from the equator, so the
   ground grid is finer than the nominal one).
2. **Symbol set for the first release: ISOM** (2017-2). The layer machinery is
   there from the start, but only one symbol table is shipped.
3. **Per-symbol zoom rules.** The symbol table of a layer carries, for each
   symbol, the rules that generalise it for coarse zoom levels, for example:
   the lowest zoom it appears at, a minimum size below which an object is
   dropped, a simplification tolerance, and whether neighbouring objects of the
   symbol may be merged. The world view depends on these rules, so they are
   part of the symbol table and not of the converter.
4. **Objects with no symbol in the global set** are mapped to the **closest
   symbol** and the converter emits a warning for each such mapping. If there
   is none, the object is dropped, also with a warning. "Closest" means: the
   same symbol code first (the ISOM number), then the same kind (point, line or
   area) with the nearest colour, size and name; the choice is deterministic and
   the warning names both symbols. If nothing is close enough it is dropped.

5. **Container: PMTiles v3, through the Rust `pmtiles` crate** (`default-features
   = false` plus `write`, in the CLI tools only, so the library and its wasm
   build stay free of tokio and the HTTP backends). Whether the crate can also
   serve the wasm reader is still to be checked; if not, the reader implements
   the small v3 directory format itself. PMTiles holds one tileset per file, so a
   layer (and the history sidecar) is either its own archive or a layer inside
   each tile's payload — see the open questions.
6. **Zoom range.** The viewer shows anything from the whole world on one screen
   down to about 10 m across the screen. In slippy-map terms that is zoom 0–2
   at the top and about zoom 24 at the bottom (about 1 cm per pixel on a
   ~1000 px screen). *Stored* levels and *displayed* levels are different
   things: vector tiles are stored only down to about zoom 14 (a tile about 2.4
   km wide at the equator), and the viewer *overzooms* them, since vectors scale.
   At the top, symbols have a per-symbol `min_zoom` (see decision 3), so the
   coarse levels (about zoom 0–10) hold essentially only the **coverage
   footprints**, which is cheap; objects appear from about zoom 11–12.
   The 5 cm grid matters only at the deepest zoom (5 cm is about 5 px at 10 m
   per screen): check on real maps that a polyline and a curve still look smooth
   there, otherwise use 1 cm.

7. **Symbol-set versioning: one layer per standard, with a versioned symbol
   table.** Symbol ids are assigned once and never renumbered; each symbol also
   keeps its standard code (101, 302.1, ...) for crosswalks. A new edition of a
   standard only adds or deprecates symbols. Maps drawn in an older edition are
   converted on import with the closest-symbol rule (decision 4), and the layer
   metadata records the edition each map was drawn in. Stored objects and their
   history therefore stay valid as the table grows. After ISOM the order is
   ISSprOM 2019 (its symbol set is already in the repo), then ISSkiOM and
   ISMTBOM; indoor has no ISO standard and needs its own table; the OSM-carto
   set comes last, being no orienteering standard.

## Open questions

1. How a layer and the history sidecar map onto PMTiles (one archive each, or layers inside the tile payload).
