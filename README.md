# rust_omap_renderer

A Rust port of `map_to_image`, the small standalone command line tool one
directory up which renders an OpenOrienteering Mapper map to a raster
image. Same input format, same rendering rules, same command line
interface — ported algorithm-for-algorithm from the C++/Qt original rather
than translated line-by-line, since Qt itself isn't available to lean on.

```
map_to_image [-r px-per-meter] [-f meters] <map-file> [image-file]
```

See `../README.md` for the option reference, exit codes, and the rendering
rules this reproduces (dash groups, mid symbols, area fill patterns, border
offsetting, text layout, ...) — all of that is unchanged here. This document
covers what's specific to the Rust port: the crate stack, what matches
exactly versus what's approximated, and how to build and test it.


## Building

```bash
cargo build --release
./target/release/map_to_image ../examples/forest\ sample.omap forest.png
```

Requires only a Rust toolchain — no Qt, no system C++ dependencies. Fonts
are resolved through the system's normal font configuration (fontconfig on
Linux) via `fontdb`.


## Crate stack

| Concern | C++ original | Here |
| --- | --- | --- |
| XML parsing | `QXmlStreamReader` | `quick-xml` |
| CLI args | `QCommandLineParser` | `clap` |
| Image encode | `QImage::save` | `image` |
| Paths, stroking, rasterization | `QPainterPath` + `QPainter` | `tiny-skia` |
| Font discovery | Qt font matching (fontconfig) | `fontdb` |
| Text shaping | Qt5's default text engine (HarfBuzz) | `rustybuzz` (a pure-Rust HarfBuzz port) |
| Glyph outlines | Qt's font engine | `ttf-parser` |

`fontdb` + `rustybuzz` + `ttf-parser` + `tiny-skia` is the same pipeline
[resvg](https://github.com/RazrFalcon/resvg) uses, which is what made text
rendering tractable here without Qt.


## Source layout

Mirrors the original's five translation units, plus one new module for text:

| File | Ported from | Contents |
| --- | --- | --- |
| `map.rs` | `map.h`, `.cpp` | The data model. `Symbol` is a Rust enum over the five symbol kinds; since Rust has no inheritance, the fields the C++ `Symbol` base class carried (`name`, `is_rotatable`, ...) are duplicated directly on each variant's struct — matters in particular for a `PointSymbol` nested inside a `LineSymbol` (its dash/mid/start/end symbol), which needs its own `is_rotatable` exactly as `PointSymbol : public Symbol` gives it one in C++. |
| `xml_reader.rs` | `xml_reader.h`, `.cpp` | A `quick_xml`-based reader with a thin normalization layer so self-closing elements present as a start immediately followed by an end token, matching `QXmlStreamReader`'s stream and keeping the parsing logic a close structural match to the original. |
| `qbezier.rs` | Qt's private `qbezier.cpp` | A line-for-line port of `QBezier::shifted()` — the curve-offset algorithm line borders are built from. The one piece of Qt-internal code the original depended on; ported from the actual Qt 5.15 source rather than reimplemented, since getting this exactly right matters for border fidelity. |
| `geometry.rs` | `geometry.h`, `.cpp` | Path flattening, extents, slicing, tangents, and `shift_coordinates` (built on `qbezier.rs`). Pure arithmetic, no rendering backend involved — this is the part of the port held to an *exact* standard, and it's covered by unit tests checking specific geometric invariants (extents of capped/joined strokes, offset direction, slice round-trips) rather than just "it compiles." |
| `renderer.rs` | `renderer.h`, `.cpp` | Turns objects into paths and draws them: dash groups, mid symbols, area fill patterns (line and point), and the color-priority draw order. Builds an internal `Path` IR (not `tiny_skia::Path` directly) so `geometry.rs` stays backend-independent; converts to `tiny_skia` only at paint time. |
| `text.rs` | `Renderer::addText()` in `renderer.cpp` | New module (the original interleaves this into `renderer.cpp`, split out here since font loading/shaping is a distinct concern). Lays out text at a large internal font size and scales down, same as the original. |
| `render.rs` | — | The map-file-to-pixel-buffer step of `main.cpp`, split out so `map_to_image` and `benchmark` render a map by the same code rather than by two copies of it. |
| `naming.rs` | — | New. The naming rules a benchmark archive has to follow, and how to repair an archive which breaks them. |
| `differences.rs` | `../benchmarks/differences.py` | New. A port of the C++ project's Python difference report, so `benchmark` writes it without needing Python, numpy and Pillow. Also measures the per-map error the results table reports, and classifies each differing pixel into one two rasterizers can legitimately disagree about and one they cannot — the one deliberate departure from the Python. |
| `progress.rs` | — | New. The `tqdm`-style progress bar the long stages draw, and the colour their headings use. Both fall back to plain lines when the output is not a terminal. |
| `report.rs` | — | New. Word wrapping for the plain text files a run writes, which are read in whatever opens a `.txt` and so arrive already folded. |
| `bin/map_to_image.rs` | `main.cpp` | The CLI. |
| `bin/benchmark.rs` | — | New. Runs a whole benchmark suite out of a zip archive, see below. |
| `bin/create_benchmark.rs` | — | New. Builds such an archive: collects the maps, has the C++ renderer draw the reference images, and names everything the way `benchmark` expects. See below. |
| `all_symbols.rs` | `create_map_with_all_symbols.cpp` | Makes one map per symbol of a map, each carrying a grid of test objects. The suites this project measures itself on are built out of these, so it is ported the same way the renderer is — and it *uses* the renderer: a cell of the grid is as wide as what its object draws, so laying the grid out means building every object's renderables to measure them. |
| `xml_writer.rs` | `Object::save()`, `XMLFileFormat` | New. Writes a map file, for `all_symbols` — the only maps this project writes are ones it has just generated. The colours and symbols go out as the source file's own bytes rather than as anything this project reassembles, so nothing a symbol holds and the renderer ignores can be lost on the way. |
| `bin/create_map_with_all_symbols.rs` | `create_map_with_all_symbols.cpp` | The CLI of the above, same arguments as the C++ tool. |


## Fidelity

Three different things had to be reproduced, at three different levels of
achievable fidelity:

1. **Pure geometry and layout math** (dash groups, mid-symbol spacing,
   path flattening, extents, border offsetting) — ported **exactly**,
   including using `f32` where Mapper measures paths in single precision,
   since the rounding decides which side of a tie a layout falls on. This
   is the part with no rendering-backend ambiguity, and it shows: the CLI's
   reported image geometry (width/height/scale) matches the C++ tool's
   **exactly**, character for character, on every benchmark map tested,
   including ones with dashed borders, area holes, and rotated fill
   patterns.
2. **`QBezier::shifted()`** — ported exactly from Qt's own source (see
   `qbezier.rs`).
3. **Qt's path stroker, antialiased rasterizer, and text/font engine** —
   approximated with `tiny-skia` and `fontdb`/`rustybuzz`/`ttf-parser`.
   Pixel-identical output was never the target here, any more than the C++
   tool is pixel-identical to Mapper itself (see `../README.md`'s own
   "Rendering fidelity" section) — different rasterizers antialias edges
   differently, and text metrics are font-engine-specific.

Measured against the same 169-map benchmark set the C++ tool documents
itself against, at a tolerance of 60 per pixel (summed over R+G+B) and a 1%
threshold: **164 of 169 pass**.

That number is a record of a measurement, not something this repository can
still reproduce on its own: it was taken with a `benchmarks.sh` driver
script and a checked-out `benchmarks/maps`/`benchmarks/expected` pair which
have since been removed in favour of `benchmark` (below), and `benchmark`
reports where renderings differ rather than passing or failing them against
a threshold. Of the 5 which did not pass:

- **2 are area-fill-pattern maps** (`138_..._stripes`, `46_..._Stony_ground`)
  at 1.22–1.24%, just over the 1% threshold. Inspected visually and via
  pixel diffs: geometry, spacing, and color all match; the difference is
  concentrated at antialiased edges (most visibly at *curved* clip-mask
  boundaries), consistent with `tiny-skia`'s rasterizer producing slightly
  different edge coverage than Qt's — not a placement or logic error. The
  antialiasing classification described below, written later and
  independently, agrees: it puts `Stony_ground` at 0.0000% and `stripes` at
  0.0003% real.
- **3 are text-heavy maps** where the rendered image is a few percent
  narrower/shorter than the reference. All three request the font family
  `"Sans Serif"` (Qt's own generic name, not CSS's `sans-serif` — both are
  recognized); the residual gap is font-metrics drift from whichever actual
  font file the reference images were rendered with versus whatever this
  system's fontconfig resolves the generic sans-serif family to. This is
  inherently system-dependent and not something the renderer logic controls.

To look at the same 5 maps, or any others, run `benchmark` over the suite
and read its difference report.

One rendering bug was found later, by the comparison in *One map per symbol*
below rather than by any of this: a text object's rotation is applied by
Mapper whether or not its text symbol is marked rotatable, and `text.rs` was
checking the symbol first. A map with a rotated text object in a
non-rotatable text symbol therefore rendered its text unrotated. Fixed;
`ISOM.omap` has one such symbol (`704 Control number`), and it is what turned
the difference up.


## One map per symbol

`create_map_with_all_symbols` takes a symbol set and writes a folder holding
one map per symbol of it, each with a grid of objects drawn with that symbol
alone, and a `.txt` next to it saying what is in each cell of the grid:

```bash
./target/release/create_map_with_all_symbols examples/ISOM.omap ISOM_symbols
```

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

It is a port of the C++ tool of the same name, and is held to that tool's
output. Over `examples/ISOM.omap`, 169 symbols:

| | |
| --- | --- |
| the `.txt` describing the grid | **169 of 169 byte-identical** |
| the symbol list of the generated map (which symbols, in which order, renumbered) | **169 of 169 identical** |
| the objects: coordinates, flags, rotations, symbol references | **158 of 169 byte-identical** |
| the image the C++ `map_to_image` draws from the generated map | **159 of 169 pixel-identical**, the rest under 0.9% of pixels |

The 11 maps whose objects differ are not shaped differently: every one is the
same shape placed a few units — thousandths of a millimetre on the paper —
away from where the C++ put it, because the extent it was centred by came out
that much different. Ten are within 20 units, which at the benchmark's own
resolution is under a pixel. Three of those are text symbols, where the
extent is a font metric and this project resolves the font itself (see
*Fidelity*); the rest are a rounded extent falling on the other side of a
whole unit. The eleventh is a 5 m circle drawn with a dashed line whose
minimum length is 79.5 m, i.e. a circle shorter than one dash of it: it lands
195 units out, and draws nothing either way — the image the C++ renderer
makes of that map is pixel-identical all the same.

Two differences that were real, both found by this comparison and both fixed:
Qt's `qRound` rounds a half **up**, not away from zero, so a shape centred on
the origin came out a unit wide in the wrong direction (`geometry::qround`);
and `TextRenderable` applies a text object's rotation whether or not its
symbol is marked rotatable, which `text.rs` was checking first — a map with a
rotated text object in a non-rotatable text symbol rendered unrotated here
and rotated in Mapper.

What the two tools write is not byte-identical, and is not meant to be: this
one copies the source file's own bytes for a symbol rather than reassembling
it from what the renderer understood of it, and it keeps every colour of the
source (a symbol names a colour by its position in the list, so dropping the
unused ones would mean renumbering the rest). The symbols themselves *are*
renumbered — a combined symbol names its parts by position, and
`CombinedSymbol::loadingFinishedEvent` rejects the whole file if one is out
of range.


## Making a benchmark archive

An archive is maps plus what the C++ renderer draws from them, so making one
means having that renderer draw them. `create_benchmark` is pointed at it and
at the maps:

```bash
cargo build --release
./target/release/create_benchmark \
    ../build/src/map_to_image \
    ../examples/forest\ sample.omap
```

The first argument is the ground truth: the C++/Qt `map_to_image` executable,
whose output the reference images are. The second says where the maps come
from, and there are two ways to answer it:

- **a folder** — searched, subfolders included, for every `.omap` and
  `.xmap` in it. This is how a suite of real maps becomes a suite.
- **a map file** — taken apart into one map per symbol, each carrying a grid
  of shapes, sizes and rotations that symbol is drawn on. That is
  `create_map_with_all_symbols`' work, and this project has its own (below),
  so nothing outside is needed for it. `--symbols-tool <path>` runs the C++
  one instead, which is how the two are checked against each other.

Either way the maps are renamed to the rules below — spaces become
underscores, and the ordinals are handed out from zero — rendered one by one,
and written to `benchmarks/benchmark_<source name>.zip`, or wherever `-o`
says. The folder is made if it is not there, and is gitignored: an archive is
a few hundred megabytes of maps which are usually not ours to distribute.

```
benchmarks/benchmark_forest_sample.zip
    benchmark_forest_sample/README.txt      what the archive is and how it was made
    benchmark_forest_sample/maps/           the maps
    benchmark_forest_sample/expected/       one reference image per map
    benchmark_forest_sample/index/          what is on each generated map
```

`-r` and `-f` are passed to the renderer, and are what the archive is ground
truth for: reference images drawn with a 50 m frame line up with nothing
drawn with a 20 m one, so `README.txt` records both, along with the renderer,
its version, the source and the date. Run the archive back with the same two.

A map the renderer cannot draw is left out rather than put in without a
reference image: the ordinals are handed out after the rendering, so what is
left has no hole in it, and `README.txt` says which maps went missing and
why. The exit code is 2 when that happened, 1 on a setup error, 0 otherwise.

`index/` is only there for a generated suite: it is the companion
description `create_map_with_all_symbols` writes for each map, saying which
symbol the map is for and what each row and column of its grid is. Nothing
reads it — it is for whoever is looking at a difference and wants to know
what they are looking at.


## Running a benchmark archive

`benchmark` takes a whole suite as a single zip file and does the render,
the compare and the difference report in one pass — one command, one
binary, no Python and no populated folders to set up first:

```bash
cargo build --release
./target/release/benchmark examples/benchmark_ISSOM.zip
```

The archive holds a `maps/` folder of `.omap` files and an `expected/`
folder with one reference image per map, either at its top level or under a
single folder:

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

Each run goes into its own folder, so runs do not overwrite each other:

```
Results/<archive name>_YYYY_MM_DD__hh_mm_ss/
    info.txt       what the run was asked to do: every setting, with what it means
    naming.txt     every naming problem and its fix, when there were any
    results.txt    a row per map: how much differs, and by how much
    predictions/   the rendered maps, one .png per map
    differences/   one folder per pair with a real difference in it
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
it runs from 0 to 765 — the measure `--tolerance` is given in, and the same
one the C++ project's `tests/image_compare.cpp` uses, so a tolerance means
the same thing in both. A pixel is *wrong* when its error is above it, and
`wrong` counts those over the union of the two images, so a size mismatch
counts as wrong everywhere.


### Antialiasing

Almost every wrong pixel in this suite is an edge that **both** renderers
drew and disagreed about the coverage of, and there are enough of them to
bury the handful that mean something. So `wrong` is split.

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
antialiasing gets no folder in `differences/` at all. On the 169-map suite
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
- **A systematic bias** — every edge coming out lighter than Qt draws it —
  would land under `antialiasing` too. That is what the `wrong` column is
  still there for, and why the split is reported rather than the raw number
  replaced.

`--keep-antialiasing` turns the whole classification off and counts every
differing pixel as real.

`mean error of wrong px` is over the wrong pixels of **both** kinds, so it
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

`Results/` is git-ignored and created on demand. Useful options:
`--names-only` checks and corrects the names without rendering anything;
`--filter TEXT` limits the run to maps whose name contains `TEXT`;
`--tolerance` sets how much a pixel may be off before it counts as wrong
(default 3, one unit per channel, which is where two renderers' float-to-
integer conversion of the same flat colour lands); `--keep-antialiasing`
reports every differing pixel instead of classifying them (see below);
`--crops`, `--crop-size`, `--zoom` and `--overview` control how much gets
cropped out and how large; `--resolution` and `--frame` are
`map_to_image`'s. `--results DIR` puts the run folder somewhere else.

The report is a Rust port (`differences.rs`) of the C++ project's
`differences.py`, and it was checked against that Python on the full 169-map
suite: the same folders, the same file names, and pixel-identical
`diff.png`, crops and overviews. Two things are equivalent rather than
identical, and both are labelling rather than measurement — the text in the
grey bars is rasterized by `tiny-skia` instead of FreeType, and the overview
in `side_by_side.png` is scaled down by the `image` crate's Lanczos filter
instead of Pillow's (measured on a downscaled overview: no pixel off by more
than 7 per channel).


## Testing

```bash
cargo test        # unit tests (geometry, qbezier, xml_reader, xml_writer,
                  # naming, all_symbols, differences) + CLI integration tests
                  # for map_to_image, benchmark, create_benchmark and
                  # create_map_with_all_symbols

./target/release/create_map_with_all_symbols examples/ISOM.omap    # one map per symbol
./target/release/create_benchmark ../build/src/map_to_image maps/  # build a suite
./target/release/benchmark suite.zip                 # a whole benchmark suite
./target/release/benchmark --filter 501 suite.zip    # only maps whose name contains "501"
```

`cargo test`'s unit tests check specific values (parsed XML fields, computed
extents, offset directions), not just "it runs" — several were written by
first checking what the real C++ `map_to_image` binary actually produces for
the same input, rather than trusting a re-derivation of the spec.
