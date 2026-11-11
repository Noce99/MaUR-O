# DEM Benchmark Viewer Plan

`dem_benchmark` (see [DEM-Benchmark-Plan.md](DEM-Benchmark-Plan.md)) scores
`contours_to_raster` on real terrain, but its numbers are a text table and
three PNGs per sample. To see *why* a sample fails -- a slope read upside
down, one wrong hill-or-hollow decision compounding over many levels -- the
results need a 3D comparison and charts. That viewer is an o-mnia extension;
this crate's part is a single file the extension can read.

Decisions (2026-11-11):

- **Input**: one zip bundle per run, written by `dem_benchmark`.
- **Audience**: a developer-only o-mnia extension, experimental, like
  `dem-contours-dev`.
- **Run comparison**: in the first version.
- **3D**: the prediction coloured by its signed error by default, with a
  swipe to the ground truth.

## Part A -- MaUR-O: `dem_benchmark` writes `bundle.zip`

Next to `summary.txt`, every run writes `bundle.zip` (unless
`--no-bundle`):

```text
bundle.zip
  results.json                   format, archive, equidistance, cell size,
                                 variants, means per variant, per-sample metrics
  sample_0001/
    sample.json                  place, box, relief, the grid (origin, cell,
                                 width, height, EPSG), per-variant metrics
    truth.f32                    ground model on the bundle grid
    water.u8                     water mask, same grid (1 = water)
    depressions/
      prediction.f32             contours_to_raster's model, same grid
      lines.json                 every contour: points, true level,
                                 predicted level, error in whole bands
    none/ ...
```

- **One grid per sample**: the ground truth's box at 2 m cells
  (`BUNDLE_CELL_M`), north-up in the sample's CRS. The truth is
  block-averaged onto it; the prediction is sampled bilinearly at its
  cell centers. So truth, prediction and water line up cell for cell and
  the viewer never resamples.
- **Grids are raw** little-endian `f32` (NaN = no data) and `u8`, row-major,
  top row first: no GeoTIFF decoder needed in the viewer.
- **Lines** are in the sample's projected CRS, rounded to 0.1 m. A line's
  `error` is its predicted level minus its true one, in whole bands, after
  the run's band offset (what the level score counts as exact, one off,
  more); `null` where the prediction does not reach it.
- **JSON is written by hand**: the crate has no serde, and the shapes are
  fixed and small.
- Library: `src/dem_bundle.rs` (grids, JSON, the zip writer);
  `dem_benchmark::LevelAccuracy` gains each line's predicted level so the
  bundle can carry it.
- `--no-bundle` skips it. Documented in the binary's doc comment, README
  and `--help` together; an integration test reads the hill's bundle back.

About 3 MB per sample (two 500 x 500 `f32` predictions, the truth, the
mask, the lines), deflated.

## Part B -- o-mnia: the `dem-benchmark` extension

A built-in, developer-only extension (`FEATURES.md` E05, experimental) in
`apps/omnia-web/src/extensions/examples/dem-benchmark/`, on its own branch
from o-mnia's `main`. Plain DOM and SVG, three.js (already a dependency)
for the 3D view, no chart library.

| File | Role |
|---|---|
| `index.ts` | The mode, tool panel, visualization panel, map layers |
| `bundle.ts` | Pure: `readZip` to a typed, validated run model |
| `stats.ts` | Pure: means, run A vs B, histograms, profiles, colour scales |
| `panel.ts` | Tool panel: load run A (and B), the sample list, the variant |
| `overview.ts` | Mean tiles, sortable table, scatter plots, paired variants, A vs B |
| `sample.ts` | One sample: 3D, histograms, cross-section |
| `scene3d.ts` | three.js: error-coloured surface, swipe to the truth, draped contours |
| `charts.ts` | Small SVG charts: scatter, histogram, profile, paired lines |
| `layers.ts` | Map layers: the error grid and the contours coloured by level error |

Views:

- **Overview**: the means per variant; a sortable per-sample table (a row
  opens the sample); RMSE against relief and inverted share against the
  number of lines; paired variants (one segment per sample from `none` to
  `depressions`); with a second run loaded, a B - A column and a
  per-sample change chart (samples matched by name, the archives checked to
  be the same).
- **Sample**: the 3D view (prediction coloured by signed error on a
  diverging scale centred on 0; a swipe plane between prediction and truth;
  contours draped a little above, coloured by level error; vertical
  exaggeration); the pixel error histogram; the level error histogram
  weighted by length; a cross-section along a drawn line, truth and
  prediction, contour crossings marked.
- **Map**: with the sample's map open (the archive's `map_*.omap`,
  opened through the host's own import), the error grid and the coloured
  contours as map layers.

Tests: Vitest on `bundle.ts` and `stats.ts` (a tiny zip built in the test);
the profile problems test covers the E05 entry. Docs:
`docs/dev/dem-benchmark.md`, the built-ins list in
`docs/dev/extension-api.md`, `FEATURES.md`.

## Order

1. MaUR-O: the bundle, its test, docs. One commit.
2. o-mnia, a branch from `main`, one commit each:
   1. the bundle reader and stats, with tests;
   2. the extension shell and the overview, with run comparison;
   3. the 3D view and the sample charts;
   4. the map layers;
   5. docs.

## Later

Tuning live: `app.terrain.computeDemFromContours()` already runs the
algorithm in the browser with config overrides. Rerunning a sample from
the viewer and scoring it against the bundle's truth needs the scoring in
the browser (exported through `maur-o-wasm`, or a light TypeScript port).
