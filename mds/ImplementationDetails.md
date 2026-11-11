# Implementation Details

This document covers what's specific to the Rust implementation: how its
crates fit together, how the source is laid out, and how the benchmarking
tools (see the main [README](../README.md#usage)) tell a genuine rendering
bug from an artifact of two rasterizers antialiasing the same edge
differently.

## Table of Contents

- [Crate stack](#crate-stack)
- [Source layout](#source-layout)
- [One map per symbol](#one-map-per-symbol)
- [Making a benchmark archive](#making-a-benchmark-archive)
- [Running a benchmark archive](#running-a-benchmark-archive)
- [Antialiasing](#antialiasing)
- [Testing](#testing)

## Crate stack

| Concern | Crate | Role |
| --- | --- | --- |
| XML parsing | `quick-xml` | Streams a map file's XML without building a DOM. |
| CLI args | `clap` | Argument parsing, `--help` and `--version` for every binary. |
| Image encode | `image` | Writes the final raster in whichever format the output suffix asks for. |
| Paths, stroking, rasterization | `tiny-skia` | Builds, strokes and fills the vector paths, and rasterizes them with antialiasing. |
| Font discovery | `fontdb` | Resolves a font family name to an actual font file, through the system's fontconfig on Linux. |
| Text shaping | `rustybuzz` | A pure-Rust HarfBuzz port; turns a string plus a font into positioned glyphs. |
| Glyph outlines | `ttf-parser` | Reads the glyph outlines `rustybuzz` shapes into paths `tiny-skia` can fill. (Pulled in through `rustybuzz`.) |
| Font resolution | `fontconfig` | Resolves a generic family name ("Sans Serif") the way Qt and Mapper do. Unix only, opened at runtime (`dlopen`), so it is not needed to build — see the [README](../README.md#fonts-and-fontconfig). |
| Archives | `zip` | Reads and writes benchmark archives, and GTFS feeds given as `.zip`. |
| Timestamps | `chrono` | The timestamped run folders and the dates recorded in `info.txt`. |
| Palette PNGs | `png` | Writes indexed (palette) PNGs — the `benchmark` diff images and `contours_to_raster`'s diagnostics — which `image` cannot. |
| Float TIFF | `tiff` | Writes `contours_to_raster`'s single-band 32-bit float elevation GeoTIFF, which `image`'s encoder cannot. |
| Contour geometry | `geo`, `geo-svg` | Polygon buffering and point-in-polygon for `contours_to_raster`; `geo-svg` draws its `--create_svg` diagnostics. |
| Scratch space | `tempfile` | The temporary folder a benchmark archive is unpacked into. |
| HTTP | `ureq` | `add_orthophoto`'s downloads from a mapping agency's WMS, and `create_dem_benchmark`'s from Lantmäteriet: blocking, rustls. Not built for wasm, and never used by the library itself. |

`fontdb` + `rustybuzz` + `ttf-parser` + `tiny-skia` is the same pipeline
[resvg](https://github.com/RazrFalcon/resvg) uses, which is what made text
rendering tractable here.

## Source layout

Everything is built on one data model — `map.rs`, what a map file says — and
every other module is a consumer of it. The crate-level documentation in
`src/lib.rs` (`cargo doc --open`) is the short, current overview; the tables
below go one file at a time.

### Reading and writing maps

| File | Contents |
| --- | --- |
| `map.rs` | The data model read from a map file. `Symbol` is a Rust enum over the five symbol kinds — line, area, point, text, combined — each carrying its own copy of the fields common to all of them (`name`, `is_rotatable`, ...), since a `PointSymbol` nested inside a `LineSymbol` (its dash/mid/start/end symbol) needs its own regardless of which symbol kind it sits inside. |
| `xml_reader.rs` | A `quick-xml`-based reader with a thin normalization layer so self-closing elements present as a start immediately followed by an end token, which keeps the parsing logic in `map.rs` simple regardless of how a particular file spells an empty element. |
| `xml_writer.rs` | Writes a map file — the only maps this project writes are ones it has just generated. For `all_symbols` the colours and symbols go out as the source file's own bytes rather than as anything this project reassembles, so nothing a symbol holds and the renderer ignores can be lost on the way. For `osm_convert`, which has no source file, `color_element`, `symbol_element` and `georeferencing_element` write them from the `map.rs` model. |
| `ocd.rs` | Reads OCAD's binary `.ocd` format (versions 6 to 12 and 2018) by converting it into the `map.rs` model, following Mapper's and Purple Pen's importers. What OCAD stores and Mapper's format has no place for is reported, not silently dropped. |
| `ocd_crs.rs` | Turns the coordinate-system number an OCAD file carries into an EPSG code, so its georeferencing survives the conversion. |

### Rendering

| File | Contents |
| --- | --- |
| `qbezier.rs` | The curve-offset algorithm line borders are built from: shifting a cubic Bézier segment sideways by a constant distance without changing its degree. Ported from Qt so borders match Mapper pixel for pixel, and covered by its own unit tests. |
| `geometry.rs` | Path flattening, extents, slicing, tangents, and `shift_coordinates` (built on `qbezier.rs`). Pure arithmetic, no rendering backend involved — the part of the renderer held to an *exact* standard, and it's covered by unit tests checking specific geometric invariants (extents of capped/joined strokes, offset direction, slice round-trips) rather than just "it compiles." |
| `renderer.rs` | Turns objects into paths and draws them: dash groups, mid symbols, area fill patterns (line and point), and the colour-priority draw order. Builds an internal `Path` IR (not `tiny_skia::Path` directly) so `geometry.rs` stays backend-independent; converts to `tiny_skia` only at paint time. |
| `text.rs` | Font loading, shaping and glyph layout, split out from `renderer.rs` since it is a distinct concern. Lays out text at a large internal font size and scales down. |
| `render.rs` | The map-file-to-pixel-buffer step (`render_map`, `render_map_window`), split out so `map_to_image` and `benchmark` render a map by the same code rather than by two copies of it. |
| `svg.rs` | Writes the renderer's own renderables out as SVG paths instead of rasterizing them — what `map_to_image` does for a `.svg` output. |

### Benchmarking

| File | Contents |
| --- | --- |
| `all_symbols.rs` | Makes one map per symbol of a symbol set, each carrying a grid of test objects. The suites this project measures itself on are built out of these, and it *uses* the renderer: a cell of the grid is as wide as what its object draws, so laying the grid out means building every object's renderables to measure them. See [One map per symbol](#one-map-per-symbol). |
| `naming.rs` | The naming rules a benchmark archive has to follow, and how to repair an archive which breaks them. |
| `archive_info.rs` | Reads back the `info.txt` `create_benchmark` writes into an archive, so `benchmark` always uses the resolution and frame the reference images were drawn at. |
| `differences.rs` | Compares two rendered images and writes the difference report `benchmark` produces. Also measures the per-map error the results table reports, and classifies each differing pixel into one two rasterizers can legitimately disagree about and one they cannot, see [Antialiasing](#antialiasing). |
| `progress.rs` | The `tqdm`-style progress bar the long stages draw, and the colour their headings use. Falls back to plain lines when the output is not a terminal. |
| `report.rs` | Word wrapping for the plain text files a run writes, which are read in whatever opens a `.txt` and so arrive already folded. |

### Contours to elevation

A fairly literal implementation of [Contours-to-Raster.md](Contours-to-Raster.md),
one file per step; read that document first.

| File | Contents |
| --- | --- |
| `contour_symbols.rs` | Classifies a map's symbols into the four families the algorithm uses: Contours, Slope Lines, Jumps and Heavy Objects. |
| `contour_geometry.rs` | Appendices 1 and 2: Bézier to equally spaced `LineString`, and `LineString` to buffered polygon. |
| `gravity_model.rs` | Step 1's data model: per-contour evidence of which side is downhill. |
| `contour_raster.rs` | The Contour Raster and its reserved pixel values, plus Appendix 4's pixel walk. |
| `step1_extract.rs` … `step4_elevation.rs` | Steps 1 to 4, in order; `step3_rain_drop.rs` is also the Rain Drop Production engine the other steps share. |
| `step5_gravity_raster.rs`, `step5_elevation_raster.rs` | Step 5: the per-pixel downhill field, then the elevation raster that follows it, and the TIFF writer. |
| `geotiff.rs` | Georeferencing for the elevation raster: affine transform and GeoTIFF tags. |
| `contours_to_raster_config.rs` | The ~35 tunable parameters, read from `config/contours_to_raster.conf` (a hand-rolled `key = value` format). |
| `contours_to_raster_svg.rs` | The `--create_svg` diagnostic files, one per stage. |
| `dem_from_contours.rs` | Steps 1 to 5 in memory, on an already-parsed map, with a progress callback and no files — the library entry point for embedding the algorithm (e.g. in a WebAssembly build). |
| `dem_geo_tiff_evaluator.rs` | Scores a predicted DEM GeoTIFF against a ground-truth one (the `dem_geo_tiff_evaluator` binary). |
| `dem.rs` | Shades a DEM (hillshading and similar) so terrain can be seen; downstream of the pipeline, not part of it. |

### DEM benchmark

`contours_to_raster` tested on real terrain: see
[DEM-Benchmark-Plan.md](DEM-Benchmark-Plan.md) for the design and the
measurements it rests on.

| File | Contents |
| --- | --- |
| `lantmateriet.rs` | What to fetch from Lantmäteriet's STAC API: search URLs, reading the answers (with a minimal JSON reader), SWEREF99 TM, the coarse Sweden outline places are drawn from, a seeded RNG, the box placed inside a point cloud tile, and which URLs may be sent credentials. No network access. |
| `dxf.rs` | A minimal ASCII DXF reader: the polylines, lines and points Karttapullautin writes. |
| `pullauta_import.rs` | Karttapullautin's DXF layers as an ISOM contour map (`101`, `101.1`, `102`, `103`, `103.1`), clipped to a box and georeferenced, plus every line's true height for `levels.txt`. |
| `dem_archive.rs` | A DEM benchmark archive's layout and reader, and the rasters derived from the ground truth: Gaussian-smoothed, and a water mask of large perfectly flat areas. |
| `dem_benchmark.rs` | The scoring: pixel errors after the mean and median offsets, per-contour level accuracy, the inverted-slope share, and a Laplace-interpolation baseline of the true levels. |
| `dem_bundle.rs` | A run as one `bundle.zip` for a viewer: every grid of a sample on one 2 m grid as raw little-endian floats, the contours with their true and predicted levels, the metrics as hand-written JSON. |

### Other consumers of the map model

| File | Contents |
| --- | --- |
| `course.rs` | Lays a course over a map: start triangle, control circles, connecting lines, numbers. |
| `runnability.rs` | Turns a map into a grid of running speeds. |
| `route.rs` | Searches a runnability grid for the fastest route between two points. |
| `snap.rs` | What a control can be placed on: the point features near a spot, and what the feature there is called. |
| `stats.rs` | Counts what a map holds: objects, symbols used, line lengths, areas. |
| `validate.rs` | Checks a map against a mapping standard (ISOM, ISSprOM). |

### OpenStreetMap to map

| File | Contents |
| --- | --- |
| `osm.rs` | Reads OpenStreetMap XML (`.osm`): nodes, ways, relations, tags; joins multipolygon rings split over several ways. |
| `osm_style.rs` | The openstreetmap-carto look at zooms 13 to 17 (the `--details` levels) as a Mapper symbol set — the colour table in carto's layer order, road casings and fills, path dashes, landcover — and the rules picking a symbol from a feature's tags. |
| `osm_convert.rs` | Projects OpenStreetMap data into UTM turned to true north, clips it to a bounding box, and writes the map with `osm_style`'s symbols. |
| `bus.rs` | Bus and train lines — route, stops, colour and midday headway of each line's main variant — read from OpenStreetMap route relations, or for buses from a GTFS feed (zip or folder), whose calendar and timetable give the headway. |
| `bundle.rs` | Lines sharing a street laid out side by side: routes matched to the street and rail network, a fixed left-to-right order carried along each bundle, offsets smoothed where a bundle changes, the band capped in width, stops as one bar across it. Pure geometry on the paper, independent of OSM. |
| `map_areas.rs` | The areas of an event's maps, read from a CSV table (`--maps`): outlines, names, forbidden zones, start and finish. |

### Orthophotos

| File | Contents |
| --- | --- |
| `orthophoto.rs` | What an orthophoto WMS is asked for: a map's place in degrees, the CRS to ask in (the map's own, else Web Mercator or WGS84), its footprint there, the north-up pixel grid and its tiles, `GetMap`/`GetCapabilities` URLs (WMS 1.1.1 or 1.3.0), capabilities parsing, the coverage test, the world file. No network access. |
| `orthophoto_services.rs` | The hand-checked list of open orthophoto WMS services across Europe, best first. |
| `image_template.rs` | Adds a georeferenced image to a Mapper file as its bottom-most template, by editing the file's text: the template list's `count` and `first_front_template`, and the saved view's `<ref template>` indices. |

### Binaries

Each one is a thin CLI over the library, documented in its own top-of-file doc
comment:

| File | Contents |
| --- | --- |
| `bin/map_to_image.rs` | Renders a single map to an image or SVG. |
| `bin/create_benchmark.rs` | Builds a benchmark archive: collects the maps, has a ground-truth renderer draw the reference images, and names everything the way `benchmark` expects. See [Making a benchmark archive](#making-a-benchmark-archive). |
| `bin/benchmark.rs` | Runs a whole benchmark suite out of a zip archive, see [Running a benchmark archive](#running-a-benchmark-archive). |
| `bin/contours_to_raster.rs` | Runs the contour-to-elevation pipeline on one map. |
| `bin/dem_geo_tiff_evaluator.rs` | Scores a predicted DEM against a ground truth. |
| `bin/osm_to_omap.rs` | Converts an `.osm` file into an `.omap`. |
| `bin/add_orthophoto.rs` | Downloads a map's orthophoto tile by tile and adds it to the map as a template. |
| `bin/create_dem_benchmark.rs` | Draws places in Sweden, downloads their lidar and ground model, runs Karttapullautin, and writes a DEM benchmark archive. |
| `bin/dem_benchmark.rs` | Runs `contours_to_raster` over a DEM benchmark archive and writes the scores, and the run's `bundle.zip`. |

## One map per symbol

`maur_o::all_symbols` takes a symbol set and writes one map per symbol of it,
each with a grid of objects drawn with that symbol alone, and a `.txt` next
to it saying what is in each cell of the grid. It has no CLI of its own —
`create_benchmark` calls it whenever it is pointed at a single map file
rather than a folder (see [Making a benchmark archive](#making-a-benchmark-archive)).

A line symbol gets a straight line, a closed polygonal square, a closed
bezier circle, an open bezier S and an open zigzag, at 5, 50 and 100 m on the
ground, plus a pair of lines around its minimum length. An area symbol gets a
square and a circle at each of the eight rotations of its fill pattern, a
square with a hole and a five-pointed star, plus a pair of squares around its
minimum area. A point symbol gets one object per rotation, a text symbol a
sample text per rotation, and a combined symbol the shapes of every
personality it contains.

The grid is what makes this more than a loop: a column is as wide as the
widest thing in it, and how wide an object is means how wide it *draws* —
line width, symbol elements and all. So the layout runs the renderer over
every object to measure it, and a generated map is laid out by the same code
which draws it.

## Making a benchmark archive

An archive is maps plus what a ground-truth renderer draws from them (see
the [`create_benchmark` usage](../README.md#create_benchmark) for its CLI
options). The naming rules below apply regardless of source — spaces become
underscores, and the ordinals are handed out from zero. Internally, an
archive looks like this:

```
benchmarks/benchmark_forest_sample_3_px_m.zip
    benchmark_forest_sample_3_px_m/info.txt        what the archive is and how it was made
    benchmark_forest_sample_3_px_m/maps/           the maps
    benchmark_forest_sample_3_px_m/expected/       one reference image per map
    benchmark_forest_sample_3_px_m/index/          what is on each generated map
```

The resolution is folded into the default archive name since an archive is
only ground truth for the resolution it was drawn at, and a folder often
ends up with more than one; the containing folder is gitignored, since an
archive is a few hundred megabytes of maps which are usually not ours to
distribute. `-r` and `-f` are what the archive is ground truth for:
reference images drawn with a 50 m frame line up with nothing drawn with a
20 m one, so `info.txt` records both, along with the renderer, its version,
the source and the date. `benchmark` has no `-r`/`-f` of its own — it always
reads them back out of `info.txt`, so a run can't drift from the resolution
and frame the reference images were actually drawn at.

A map the renderer cannot draw is left out rather than put in without a
reference image: the ordinals are handed out after the rendering, so what is
left has no hole in it, and `info.txt` says which maps went missing and why.

`index/` is only there for a generated suite: it is the companion
description `maur_o::all_symbols` writes for each map, saying which symbol the
map is for and what each row and column of its grid is. Nothing reads it —
it is for whoever is looking at a difference and wants to know what they are
looking at.

## Running a benchmark archive

`benchmark` takes a whole suite as a single zip file (see the
[`benchmark` usage](../README.md#benchmark) for its CLI options). The archive
holds a `maps/` folder of `.omap` files and an `expected/` folder with one
reference image per map, either at its top level or under a single folder:

```
maps/000__001_line_101_Contour.omap        expected/000__001_line_101_Contour.png
maps/001__002_point_101.1_Slope_line.omap  expected/001__002_point_101.1_Slope_line.png
```

Every name starts with a zero padded ordinal, then `__`, then a name free of
spaces. The ordinals run from zero upwards with no hole and no repeat, all
padded to the same width, so that the suite sorts the same way as text as it
does as numbers — sorted as text, `01_a` otherwise comes before `100_a`
comes before `02_a`.

An archive which breaks the rules is not rejected: a corrected copy is
written next to it as `<name>_corrected.zip` and the run carries on with the
corrected names. What each name should have been, and why, goes into
`naming.txt` in the run folder — a suite whose ordinals are all one off is a
few hundred near-identical lines, which is a file, not a thing to scroll
past — and the terminal gets one line saying how many there were and where
they are. Nothing is invented: a map with no reference image, or a reference
image with no map, is reported and left alone.

The run announces its three stages — checking the names, rendering, and
comparing — and draws a `tqdm`-style progress bar through the two long ones,
so a couple of hundred maps say how far along they are and how long they
have left rather than scrolling a line per map.

Each run goes into its own timestamped folder, so runs do not overwrite each
other; alongside the `results.txt`/`predictions/`/`differences/` the
[`benchmark` usage](../README.md#benchmark) already mentions, it also holds:

```
Results/<archive name>_YYYY_MM_DD__hh_mm_ss/
    info.txt       what the run was asked to do: every setting, with what it means
    naming.txt     every naming problem and its fix, when there were any
```

`results.txt` is the table worth keeping from a run, sorted by its first
column, worst first, so the maps worth looking at are at the top:

```
   real  antialiasing    wrong  largest  mean error of wrong px  map
-------  ------------  -------  -------  ----------------------  ---------------------------
0.0331%       0.4888%  0.5219%       33             8.60 ± 5.35  082__083_area_406.1_Vegetat.
0.0269%       0.7087%  0.7356%      126           27.74 ± 20.02  003__004_text_102.1_Contour.
0.0000%       1.4199%  1.4199%      570           45.88 ± 38.40  108__109_line_502_Wide_road
0.0000%       0.8091%  0.8091%      259           43.65 ± 38.33  000__001_line_101_Contour
```

The error of a pixel is its difference summed over red, green and blue, so
it runs from 0 to 765 — the measure `--tolerance` is given in. A pixel is
*wrong* when its error is above it, and `wrong` counts those over the union
of the two images, so a size mismatch counts as wrong everywhere. Why `wrong`
is further split into `real` and `antialiasing` is explained next.

`mean error of wrong px` is over the wrong pixels of both kinds, so it
mostly says how far apart the two rasterizers are along an edge. Every pixel
at or below the tolerance is left out of it. An average over all pixels
would instead mostly measure how much blank paper a map has, since the two
renderings agree on all of it; pixels outside the overlap count as wrong but
have no error to measure, so they are not in the average either. It reads
`n/a` where no pixel was wrong at all.

`info.txt` records the settings the run used — resolution, frame, tolerance,
crops, crop size, zoom, overview, filter — each with a paragraph on what it
does and what changing it would mean, so a table read months later still
says which measurement it is.

A `differences/` folder holds, for each pair which differs:

```
side_by_side.png       both images whole, expected left, predicted right
diff.png               black where they agree, red where they do not
crop_01_2888x1352.png  the worst region: expected, predicted and diff, enlarged
crop_02_2856x1432.png  the second worst region, and so on
```

There are as many crops as it takes to cover **every** difference, worst
region first, and their names carry the top left corner of the region so it
can be found again in the full size images.

`Results/` is git-ignored and created on demand.

## Antialiasing

Almost every wrong pixel in a benchmark suite is an edge that **both**
renderers drew and disagreed about the coverage of, and there are enough of
them to bury the handful that mean something. That's the problem this
classification solves: it separates the edge noise from the differences
actually worth a human looking at.

Along an edge a pixel's colour is a blend of what lies on either side, mixed
in proportion to how much of the pixel the shape covers, and two rasterizers
work that coverage out differently. So a pixel counts as `antialiasing` when
**both** renderings have a colour step in the 3×3 window around it — when
both of them drew an edge there. Requiring it of both is what carries the
weight: where only one has an edge, one of them drew something the other did
not. Everything else is `real`: a missing symbol, an extra mark, a shape
well out of place, an area filled in the wrong colour, or a rendering of the
wrong size.

An earlier version of this asked something stricter, and it is worth
recording why it failed. Since a coverage disagreement can only remix
colours that are both present, each image's colour ought to lie inside the
range the other takes in the window. That reads well and does not work: it
needs the colours being mixed to be *visible* nearby, and a map is full of
features thinner than a pixel. At a road's casing the edge pixels come out
59% asphalt, 24% white and 17% black — from a gap of white paper narrower
than one pixel, so pure white appears nowhere in the neighbourhood at any
window size, and the blend sits outside the range of everything on either
side of it. That test reported road casings across a quarter of the suite as
real differences.

The classification decides what you actually look at: `real` pixels alone
choose which regions get cropped out, `antialiasing` ones are drawn dim
orange rather than red in `diff.png`, and a pair whose every difference is
antialiasing gets no folder in `differences/` at all. On a 169-map suite
that turns 168 folders into 36 and 171 MB into 24 MB; the contour map's 52
crops become none at all, and what is left at the top of the table is the
text maps whose metrics genuinely differ.

Three limits, all worth knowing:

- **A shape drawn under a pixel out of place has exactly the signature of a
  coverage disagreement**, and no local test can separate them. The 3×3
  window is therefore also the statement of how much positional disagreement
  is forgiven: one pixel, about as far as either rasterizer spreads an edge.
- **A colour error confined to the pixels of an edge** is forgiven for the
  same reason. Over any area more than a pixel wide the interior has no edge
  in either image, so it is still reported.
- **A systematic bias** — every edge coming out lighter than the reference
  draws it — would land under `antialiasing` too. That is what the `wrong`
  column is still there for, and why the split is reported rather than the
  raw number replaced.

`--keep-antialiasing` turns the whole classification off and counts every
differing pixel as real — see [`benchmark`'s options](../README.md#benchmark).

`differences.rs`'s output was checked pixel-for-pixel against a companion
Python difference-report tool, on a 169-map suite: the same folders, the
same file names, and pixel-identical `diff.png`, crops and overviews. Two
things are equivalent rather than identical, and both are labelling rather
than measurement — the text in the grey bars is rasterized by `tiny-skia`
instead of FreeType, and the overview in `side_by_side.png` is scaled down
by the `image` crate's Lanczos filter instead of Pillow's (measured on a
downscaled overview: no pixel off by more than 7 per channel).

## Testing

```bash
cargo test --all-targets --locked     # unit + integration tests
cargo test --doc --locked             # doc tests (the example in lib.rs)
cargo test --test cli                 # one integration test file
cargo test <name>                     # tests whose name contains <name>
```

- **Unit tests** live next to the code they test, in `#[cfg(test)]` modules.
  Those for the pure-arithmetic modules (`geometry.rs`, `qbezier.rs`) check
  specific geometric invariants — extents, offset direction, slice
  round-trips — rather than just that something runs.
- **Integration tests** in `tests/` drive the **built binaries** through
  `assert_cmd` wherever the command line interface is the contract being
  tested: the exit codes are documented and scripted against, so a change to
  one of them is a breaking change no library-level test would notice.
  Others (`tests/ocd.rs`, `tests/route.rs`, ...) exercise library modules
  end to end.
- **Fixtures** are in `tests/data/`: small `.xmap` maps, an OpenStreetMap
  extract (`bologna_sample.osm`) and a GTFS feed (`gtfs_sample/`).
- **Regressions**: inputs that once crashed a tool, lost part of a map or
  gave a wrong answer are pinned in `tests/regressions.rs`, each as a small
  hand-written map holding just the thing that went wrong.

CI (`.github/workflows/ci.yml`) runs the tests on Linux, macOS and Windows,
plus `cargo fmt --check`, `cargo clippy -D warnings`, `cargo doc` with
warnings as errors (every public item must have a doc comment), and a build on
the minimum supported Rust version, 1.88.

The benchmarks themselves are not part of the test suite: they need an
external ground-truth renderer and maps that are not in the repository. How to
reproduce the public ones is in [bug.md](bug.md#tested-benchmarks). Likewise
the DEM benchmark: `tests/dem_benchmark.rs` pins `create_dem_benchmark`'s
exits before the network and runs `dem_benchmark` on a synthetic hill, but
building a real archive needs a Lantmäteriet account and Karttapullautin.
