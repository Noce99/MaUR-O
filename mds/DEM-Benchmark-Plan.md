# DEM Benchmark Plan

A benchmark for `contours_to_raster` on real terrain: pick random locations in
Sweden that have real relief, download each one's Lantmäteriet lidar point
cloud (LAZ) and 1 m ground model (DTM), run **Karttapullautin** on the point
cloud for realistic orienteering contours, import those contours into an
`.omap`, run `contours_to_raster` on it, and score the result against the DTM,
both with RMSE and per contour line.

It copies `create_benchmark` → `benchmark`: one networked step that freezes a
dataset, and one offline step that scores against it as often as you like.

## What is already known

From a spike on one tile near Göteborg (2026-11-10):

- **STAC search**: `GET https://api.lantmateriet.se/stac-hojd/v1/search?bbox=w,s,e,n&limit=N`
  (WGS84 bbox, HTTP Basic auth with a personal GeoTorget account, the same
  `LANTMATERIET_USERNAME` / `LANTMATERIET_PASSWORD` o-mnia uses). It returns
  two collections:

  | Collection | What it is | Tile | Format |
  |---|---|---|---|
  | `dtm-cog` | Markhöjdmodell, the 1 m ground model (CC-BY-4.0) | 10 × 10 km here, ~250 MB | Cloud-optimized GeoTIFF |
  | `dsm-skoglig-copc` | Laserdata Skog point cloud (licence "other") | 2.5 × 2.5 km, ~75 MB | COPC `.laz` (LAS 1.4, point format 6) |

- **Downloads** (`https://dl1.lantmateriet.se/hojd/data/...`) need the same
  Basic auth. **Range requests are refused (403)**: only whole-file GETs
  work, so tiles are downloaded whole and cached. The first requests of a
  session sometimes 403 too and succeed on retry, so the downloader retries
  with backoff. The `.../hojd/pub/...` sidecars (`*_info.json`, thumbnails)
  need no auth.
- **The LAZ and DTM come from different years** (here 2019 and 2023).
- **Karttapullautin** (Rust `pullauta`, v2.16) reads the COPC LAZ as-is,
  finds ground-classified points, and processes a whole 2.5 km tile in about
  10 s. With its default ini (`output_dxf=1`) it writes plain DXF to `temp/`:
  `out2.dxf` (layers `contour`, `contour_index`, `contour_intermed`,
  `depression`, `depression_index`, `depression_intermed`,
  `small_depression`, `slope_line`), `formlines.dxf` (`formline`) and
  `dotknolls.dxf` (points).

## Architecture

**Two new binaries:**

| Binary | Network | What it does |
|---|---|---|
| `create_dem_benchmark` | yes (`ureq`), plus external `pullauta` | sample locations → download → Karttapullautin → import → write archive |
| `dem_benchmark` | no | run `dem_from_contours::compute` on each sample → score → report under `Results/` |

**New library modules.** All are pure, and none touches the network, so the
wasm build is unaffected:

- **`lantmateriet.rs`**: STAC search URL, response parsing (split LAZ from DTM
  assets, read the year and bbox), the `info.json` boundary and density,
  SWEREF99 TM, and the tile-selection logic. Same split as `orthophoto.rs`:
  it says *what* to download, and the binary does the HTTP.
- **`dxf.rs`**: a minimal ASCII DXF reader (`POLYLINE`/`VERTEX`/`SEQEND`,
  `LWPOLYLINE`, `POINT`, layer names), just what Karttapullautin writes.
- **`pullauta_import.rs`**: DXF layers → `map.rs` objects with ISOM symbols,
  clipped to the sample box, georeferenced in EPSG:3006.
- **`dem_benchmark.rs`**: the scoring (masks, offsets, per-contour level
  accuracy, inverted area, baseline).

**Existing code reused:** `dem_geo_tiff_evaluator::{read, evaluate}` and its
PNG writers, `dem_from_contours::compute`, `xml_writer`, `contour_symbols`
(codes 101 / 102 / 103, slope lines 101.1 / 103.1), and the transverse
Mercator in `osm_convert` (SWEREF99 TM is TM with central meridian 15°,
k = 0.9996, false easting 500 000).

## Phases

### Phase 0: settle the open facts

On the spike tile, confirm:

- **DXF coordinates**: absolute SWEREF99 TM meters?
- **What the layers mean**: sample the DTM along each layer's lines. Which
  interval is `contour`, `contour_index`, `contour_intermed`, `formline`?
- **Point cloud vs DTM**: how well do heights agree across the year gap?

#### Phase 0 findings

Measured on LAZ tile `19b002-63975_3175_25` (2019) against DTM tile
`639_31` (2023), by sampling the DTM along every DXF polyline:

| DXF layer | Lines | Level | DTM − DXF height (median / p90 abs) |
|---|---|---|---|
| `contour` | 419 (368 closed) | multiples of 5 m | −0.26 m / 1.50 m |
| `contour_index` | 92 | multiples of 25 m | −0.17 m / 1.24 m |
| `contour_intermed` | 524 | odd multiples of 2.5 m | −0.22 m / 1.46 m |
| `depression` / `_index` / `_intermed` | 7 / 5 / 11 rings | as above | ≤ 0.1 m / ≤ 0.5 m |
| `slope_line` | 23 two-point 6 m ticks | the level of the ring they sit on | |
| `small_depression` | 65 | a fixed 8-vertex 18.7 m glyph, not a contour | |
| `formline` (`formlines.dxf`) | 721, **all open**, no height | exact pieces of `contour_intermed` / `depression_intermed` | |

- **DXF coordinates are absolute SWEREF99 TM meters**, and each polyline's
  height is its group code 38 (each vertex repeats it as code 30). So every
  imported line comes with its true level for free; the DTM is not needed to
  label them.
- **Depression rings are their own layers**, not duplicated on `contour`.
  Each one carries exactly one `slope_line` tick (23 ticks for 23 rings).
- **The point cloud and DTM agree** to about a meter despite the four-year
  gap, so on this tile the year gap is not a concern (still recorded per
  sample).
- **How `contours_to_raster` reads the input** (`step1_extract.rs`): a slope
  line is a **point** object (101.1 / 103.1) whose rotation `r` gives the
  downhill direction `(-sin r, -cos r)` in map coordinates (y down). It is
  attributed to the nearest contour within
  `slope_lines_contours_search_radius`. **Open form lines are dropped**, so
  Karttapullautin's form lines (all open) never reach the algorithm. A closed
  contour with no slope line and nothing inside it is assumed to be a hill,
  which is exactly what the `none` variant gets wrong for depressions.
- **The crate's own GeoTIFF reader** (`dem_geo_tiff_evaluator::read`) opens
  Lantmäteriet's encoding (tiled, deflate, floating-point predictor, NoData
  −9999), so no GDAL is needed. It reads a whole file as `f64`, though: a
  10 000 × 10 000 DTM tile is 800 MB in memory, so cropping to the sample box
  needs a windowed read of just the TIFF tiles it overlaps.

### Phase 1: downloader and sampler

`lantmateriet.rs` + `create_dem_benchmark`. Credentials from the
environment; missing ones are a documented exit code. Credentials are only
ever sent to `https://*.lantmateriet.se`.

For each sample (seeded RNG, `--seed`, `--samples N`, `--size 1000` m):

1. **Pick a point** uniformly within a coarse Sweden outline.
2. **STAC search** around it and pick the LAZ tile containing it. Place the
   box inside that tile with a margin, so one LAZ tile is enough.
3. **Cheap first filter** on `info.json` (no auth): reject low point density.
4. **Download the LAZ** (whole, retried) and **run `pullauta`**
   (`--pullauta <path>`, required) in a temp dir.
5. **Relief filter**: require enough contour lines crossing the box;
   otherwise reject and resample. Reject reasons are counted in `info.txt`.
6. **Download the DTM** tile(s) covering the box and check the year gap
   against the LAZ (`--max-year-gap`).
7. **Cache** downloads under `--cache` (default `dem_cache/`, gitignored).

### Phase 2: DXF import

`dxf.rs` + `pullauta_import.rs`.

| DXF layer | Becomes |
|---|---|
| `contour`, `depression` | 101 Contour |
| `contour_index`, `depression_index` | 102 Index contour |
| `formline` | 103 Form line (open, so ignored by the algorithm, but on the map) |
| `depression_intermed` | 103 Form line ring, only where a `formline` runs along it (that is when Karttapullautin draws it) |
| `slope_line` | a 101.1 / 103.1 point at the tick's end on the ring, rotated to point along the tick (into the depression); dropped if its ring was not imported |
| `contour_intermed` | skipped (the 2.5 m lines `formline` is selected from) |
| `small_depression`, dot knolls | skipped |

Every imported line keeps its DXF height in a sidecar (`levels.txt`: object
index → level in meters), which is what per-contour scoring is measured
against.

**Variants**, written as separate maps per sample: `depressions` (slope
lines on depressions, as a real map would have) and `none` (everything
inferred from shape).

Lines are clipped to the box; the map is georeferenced in EPSG:3006 at
1:10 000 with a 5 m equidistance.

### Phase 3: archive format

```
<archive>/
  info.txt                      seed, size, equidistance, pullauta version, sample count, reject stats
  sample_0001/
    map_depressions.omap
    map_none.omap
    ground_truth.tif            the DTM cropped to the box
    ground_truth_smoothed.tif   Gaussian-smoothed DTM
    levels.txt                  every imported line's true level
    water_mask.tif              large perfectly flat areas
    preview.png                 Karttapullautin's own render
    sample.txt                  center (WGS84 + 3006), tile ids, years
```

No raw LAZ or full DTM tiles go into the archive; they stay in the cache.

### Phase 4: scoring

For each sample and variant: run `compute`, time it, and record failures
(a Step 2 gravity conflict) as failures, not crashes.

- **Masks**: water, an edge margin (`--margin`, default 50 m).
- **RMSE / MAE**, in meters and in equidistances, with the offset fitted
  both by mean (what `evaluate` does) and by median (robust to an inverted
  hill).
- **Per-contour level accuracy**: each contour's true level is its DXF height
  (`levels.txt`); its predicted level is the median of the prediction along
  it. After the best integer offset (the
  mode of differences): % exactly right, % off by one, % off by more, and
  the share of contour *length* that is wrong.
- **Inverted area**: % of pixels whose predicted slope direction disagrees
  with the ground truth's by more than 90°, where both have a real slope.
- **Baseline**: the same contours at their *true* levels, filled by Laplace
  interpolation. The gap between baseline and prediction is the cost of
  inferring the levels.
- Against both the raw and the smoothed DTM.

Report: `Results/dem_benchmark_<timestamp>/`, one folder per sample/variant
(metrics, error PNGs) and a `summary.txt` table with aggregates per variant.

### Phase 5: tests, docs

Unit tests for DXF parsing, the layer mapping, STAC parsing (a trimmed real
response as fixture), SWEREF99 TM, level matching and the Laplace baseline.
Integration tests only on the offline exit paths (missing credentials,
missing `pullauta`, a tiny hand-made archive through `dem_benchmark`), so
CI never touches the network or needs Karttapullautin. Docs: both binaries'
top-of-file docs, README, CLAUDE.md, ImplementationDetails.md.

## Open questions

1. Should mountain samples use a bigger equidistance, or be excluded?
2. **Licence**: Laserdata Skog is "other", not CC. Archives stay local and
   gitignored; check before sharing archives with maps in them.
3. Should `dem_benchmark` fail (non-zero exit) below a level-accuracy
   threshold, or only report?

## Status (2026-11-10)

All five phases are implemented: `create_dem_benchmark`, `dem_benchmark`,
and the modules above, with unit tests and `tests/dem_benchmark.rs`.

Where the build differs from the plan:

- **Levels come from the DXF**, not from sampling the DTM (Phase 0 showed
  every Karttapullautin line carries its height).
- **The year gap** compares the ground model's `ursprungligtmatdatum` (when
  its lidar was flown) with the point cloud's `datetime`. The ground model's
  own `datetime` is when it was processed: 2023 for a tile flown in 2019.
- **A sample's box always fits one ground model tile**: point cloud tiles
  (2.5 km) nest inside the 10 km ground model grid, and the box is kept
  100 m inside its point cloud tile.
- **`--at LAT,LON`** pins places instead of drawing them, to reproduce a
  sample.
- **Water** is only the ground model's flat areas; the point cloud's water
  class is not used.
- **The ground model is cut with a windowed read** of just the TIFF tiles
  the box needs (`dem_geo_tiff_evaluator::read_window`, 46 ms instead of
  800 MB for a 10 km tile).

### First result

The spike tile's hilliest square kilometer (`--at 57.6922163,11.9632691`,
151 contour lines, 75 m of relief), default config:

| Variant | RMSE | MAE | Levels exact | Wrong length | Inverted | Baseline RMSE |
|---|---|---|---|---|---|---|
| `depressions` | 7.82 m | 5.78 m | 27.6% | 69.9% | 28.8% | 0.67 m |
| `none` | 13.83 m | 12.18 m | 18.4% | 79.7% | 34.8% | 0.67 m |

The baseline shows these contours support well under a meter, so almost
all of the error is in inferring the levels: the predicted levels along the
lines sit on whole bands (their fractional parts cluster at 0, and at 0.5
for form lines), but spread from 7 bands below to 2 above the best offset.

### Open issues

- **Downloads were refused (403) for at least the 15 minutes after the
  first ~1 GB**, for every tile, while the STAC API kept answering and
  wrong credentials still got 401. It looks like a per-account download
  limit; the early 403s of the spike, which cleared on retry, may have been
  the same thing. `create_dem_benchmark` counts each such place as
  rejected ("a tile could not be downloaded") and moves on, so a run that
  hits the limit ends with exit code 6 having kept what it got; rerun it
  later with the same `--cache` and seed to pick up where it left off.
- Questions 1–3 above are still open.
