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

The answers are recorded below, under *Phase 0 findings*.

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
| `formline` | 103 Form line |
| `depression*`, `small_depression` | also get slope lines (101.1 / 103.1), ticks pointing into the ring |
| `contour_intermed`, `depression_intermed` | skipped (the 2.5 m lines `formline` is selected from) |
| dot knolls, `slope_line` | skipped |

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
- **Per-contour level accuracy**: each contour's true level is the median of
  the DTM along it, snapped to the equidistance grid; its predicted level is
  the median of the prediction along it. After the best integer offset (the
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
