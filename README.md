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
| `differences.rs` | `../benchmarks/differences.py` | New. A port of the C++ project's Python difference report, so `benchmark` writes it without needing Python, numpy and Pillow. Also measures the per-map error the results table reports. |
| `progress.rs` | — | New. The `tqdm`-style progress bar the long stages draw, and the colour their headings use. Both fall back to plain lines when the output is not a terminal. |
| `report.rs` | — | New. Word wrapping for the plain text files a run writes, which are read in whatever opens a `.txt` and so arrive already folded. |
| `bin/map_to_image.rs` | `main.cpp` | The CLI. |
| `bin/benchmark.rs` | — | New. Runs a whole benchmark suite out of a zip archive, see below. |


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
  different edge coverage than Qt's — not a placement or logic error.
- **3 are text-heavy maps** where the rendered image is a few percent
  narrower/shorter than the reference. All three request the font family
  `"Sans Serif"` (Qt's own generic name, not CSS's `sans-serif` — both are
  recognized); the residual gap is font-metrics drift from whichever actual
  font file the reference images were rendered with versus whatever this
  system's fontconfig resolves the generic sans-serif family to. This is
  inherently system-dependent and not something the renderer logic controls.

To look at the same 5 maps, or any others, run `benchmark` over the suite
and read its difference report.


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
    differences/   one folder per differing pair
```

`results.txt` is the table worth keeping from a run — the share of pixels
which differ, the worst single pixel, the mean error with its standard
deviation, and the map — sorted by the first column, worst first, so the
maps worth looking at are at the top:

```
   wrong  largest  mean error of wrong px  map
--------  -------  ----------------------  ------------------------------------------
20.9975%       26             1.05 ± 0.66  055__056_area_302.1_Shallow_body_of_water
20.2424%      152             1.47 ± 3.56  054__055_combined_302_Shallow_body_of_wa...
 5.6640%      120           40.08 ± 24.41  045__046_area_212_Stony_ground__fight
 0.0000%        0                     n/a  080__081_area_405_Forest
```

The error of a pixel is its difference summed over red, green and blue, so
it runs from 0 to 765 — the measure `--tolerance` is given in, and the same
one the C++ project's `tests/image_compare.cpp` uses, so a tolerance means
the same thing in both. A pixel is *wrong* when its error is above it. `wrong`
counts those over the union of the two images, so a size mismatch counts as
wrong everywhere. The mean and deviation are over the **wrong pixels alone**:
every pixel at or below the tolerance is left out, which at the default
tolerance of 0 means every pixel that matches exactly. That is what makes
the column comparable between maps — an average over all pixels would mostly
measure how much blank paper a map has, since the two renderings agree on
all of it. Pixels outside the overlap are counted as wrong but have no error
to measure, so they are not in the average either; it reads `n/a` where no
pixel was wrong at all.

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
`--tolerance` sets how much a pixel may be off before it counts as wrong, and
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
cargo test        # unit tests (geometry, qbezier, xml_reader, naming, differences)
                  # + CLI integration tests for map_to_image and benchmark

./target/release/benchmark suite.zip                 # a whole benchmark suite
./target/release/benchmark --filter 501 suite.zip    # only maps whose name contains "501"
```

`cargo test`'s unit tests check specific values (parsed XML fields, computed
extents, offset directions), not just "it runs" — several were written by
first checking what the real C++ `map_to_image` binary actually produces for
the same input, rather than trusting a re-derivation of the spec.
