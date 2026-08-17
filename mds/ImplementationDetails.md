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
| CLI args | `clap` | Argument parsing, `--help` and `--version` for all three binaries. |
| Image encode | `image` | Writes the final raster in whichever format the output suffix asks for. |
| Paths, stroking, rasterization | `tiny-skia` | Builds, strokes and fills the vector paths, and rasterizes them with antialiasing. |
| Font discovery | `fontdb` | Resolves a font family name to an actual font file, through the system's fontconfig on Linux. |
| Text shaping | `rustybuzz` | A pure-Rust HarfBuzz port; turns a string plus a font into positioned glyphs. |
| Glyph outlines | `ttf-parser` | Reads the glyph outlines `rustybuzz` shapes into paths `tiny-skia` can fill. |

`fontdb` + `rustybuzz` + `ttf-parser` + `tiny-skia` is the same pipeline
[resvg](https://github.com/RazrFalcon/resvg) uses, which is what made text
rendering tractable here.

## Source layout

One file per concern, plus a few modules only the benchmarking tools need:

| File | Contents |
| --- | --- |
| `map.rs` | The data model read from a map file. `Symbol` is a Rust enum over the five symbol kinds — line, area, point, text, combined — each carrying its own copy of the fields common to all of them (`name`, `is_rotatable`, ...), since a `PointSymbol` nested inside a `LineSymbol` (its dash/mid/start/end symbol) needs its own regardless of which symbol kind it sits inside. |
| `xml_reader.rs` | A `quick-xml`-based reader with a thin normalization layer so self-closing elements present as a start immediately followed by an end token, which keeps the parsing logic in `map.rs` simple regardless of how a particular file spells an empty element. |
| `qbezier.rs` | The curve-offset algorithm line borders are built from: shifting a cubic Bézier segment sideways by a constant distance without changing its degree. Precise enough to warrant its own module and its own unit tests, since getting it exactly right matters for border fidelity. |
| `geometry.rs` | Path flattening, extents, slicing, tangents, and `shift_coordinates` (built on `qbezier.rs`). Pure arithmetic, no rendering backend involved — the part of the renderer held to an *exact* standard, and it's covered by unit tests checking specific geometric invariants (extents of capped/joined strokes, offset direction, slice round-trips) rather than just "it compiles." |
| `renderer.rs` | Turns objects into paths and draws them: dash groups, mid symbols, area fill patterns (line and point), and the colour-priority draw order. Builds an internal `Path` IR (not `tiny_skia::Path` directly) so `geometry.rs` stays backend-independent; converts to `tiny_skia` only at paint time. |
| `text.rs` | Font loading, shaping and glyph layout, split out from `renderer.rs` since it is a distinct concern. Lays out text at a large internal font size and scales down. |
| `render.rs` | The map-file-to-pixel-buffer step, split out so `map_to_image` and `benchmark` render a map by the same code rather than by two copies of it. |
| `naming.rs` | The naming rules a benchmark archive has to follow, and how to repair an archive which breaks them. |
| `differences.rs` | Compares two rendered images and writes the difference report `benchmark` produces. Also measures the per-map error the results table reports, and classifies each differing pixel into one two rasterizers can legitimately disagree about and one they cannot, see [Antialiasing](#antialiasing). |
| `progress.rs` | The `tqdm`-style progress bar the long stages draw, and the colour their headings use. Falls back to plain lines when the output is not a terminal. |
| `report.rs` | Word wrapping for the plain text files a run writes, which are read in whatever opens a `.txt` and so arrive already folded. |
| `bin/map_to_image.rs` | The CLI for rendering a single map. |
| `bin/benchmark.rs` | Runs a whole benchmark suite out of a zip archive, see [Running a benchmark archive](#running-a-benchmark-archive). |
| `bin/create_benchmark.rs` | Builds such an archive: collects the maps, has a ground-truth renderer draw the reference images, and names everything the way `benchmark` expects. See [Making a benchmark archive](#making-a-benchmark-archive). |
| `all_symbols.rs` | Makes one map per symbol of a symbol set, each carrying a grid of test objects. The suites this project measures itself on are built out of these, and it *uses* the renderer: a cell of the grid is as wide as what its object draws, so laying the grid out means building every object's renderables to measure them. |
| `xml_writer.rs` | Writes a map file, for `all_symbols` — the only maps this project writes are ones it has just generated. The colours and symbols go out as the source file's own bytes rather than as anything this project reassembles, so nothing a symbol holds and the renderer ignores can be lost on the way. |

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