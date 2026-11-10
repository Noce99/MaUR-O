# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

MaUR-O (**Ma**p **U**tils in **R**ust for **O**rienteering) renders OpenOrienteering
Mapper `.omap`/`.xmap` files (and OCAD `.ocd` files, converted to Mapper's format on
the way in) to raster images in pure Rust — no Qt, no Mapper, no graphical
environment. Alongside the renderer it ships benchmarking tools that compare its
output against a ground-truth renderer, and a second, independent pipeline
(`contours_to_raster`) that extracts elevation/gravity information from a map's
contour lines. A sixth binary, `osm_to_omap`, goes the other way: it turns
OpenStreetMap data (plus optional GTFS transit feeds) into a Mapper map. A
seventh, `add_orthophoto`, downloads the aerial orthophoto of a map's ground
(Emilia-Romagna's AGEA WMS, for now) and adds it to the map as a template.

This crate is consumed by the o-mnia web app as a git submodule (via
`crates/maur-o-wasm`, o-mnia's own wasm-bindgen adapter — see that repo's
`docs/dev/rust-backends.md`). Keep this crate a general-purpose **algorithms
library plus its own CLI tools** — no UI, no o-mnia app policy (Settings,
profiles, project files, extension contracts, worker/store plumbing). A new
capability an app needs is new library/CLI surface here; the app-facing
wiring (wasm export, job protocol, UI) belongs in o-mnia, not here.

## Commands

```bash
cargo build --release                          # build everything; binaries land in target/release/
cargo test --all-targets --locked              # run all tests (unit + integration)
cargo test --doc --locked                      # doc tests (the render_map example in lib.rs)
cargo test <test_name>                         # run a single test by name (substring match)
cargo test --test cli                          # run just one integration test file (tests/cli.rs)
cargo fmt --all --check                        # formatting check (CI-enforced)
cargo clippy --all-targets --locked -- -D warnings   # lints (CI-enforced, warnings are errors)
cargo doc --no-deps --locked                   # doc build; every public item must have a doc comment
                                                # (RUSTDOCFLAGS=-D warnings in CI, #![warn(missing_docs)] in lib.rs)
```

CI (`.github/workflows/ci.yml`) runs the above across Linux/macOS/Windows plus an
MSRV check pinned to Rust 1.88 (the floor set by the `image` crate). There is no
Cargo workspace — one crate, seven binaries.

### The seven binaries

| Binary | Purpose |
| --- | --- |
| `map_to_image` | Render a single map to PNG/BMP/TIFF/JPEG, or to SVG. |
| `create_benchmark` | Build a benchmark archive (maps + ground-truth reference images) from a folder of maps or a single map (in which case one test map per symbol is generated). |
| `benchmark` | Run a benchmark archive: render every map, diff against the reference, write a report under `Results/`. |
| `contours_to_raster` | Run the 5-step elevation-extraction pipeline (see below) on one map, writing an elevation GeoTIFF. |
| `dem_geo_tiff_evaluator` | Score a predicted DEM GeoTIFF against a ground-truth one (MSE/RMSE/MAE + error maps). |
| `osm_to_omap` | Convert OpenStreetMap XML (and optionally GTFS feeds) into an `.omap` styled like openstreetmap-carto. |
| `add_orthophoto` | Download a map's aerial orthophoto (Emilia-Romagna AGEA WMS only, for now) and add it to the map as a georeferenced template. |

Full CLI flags for all seven are documented in each binary's own top-of-file doc
comment (`src/bin/*.rs`) and in `README.md`'s Usage section. When changing a
flag, default or exit code, update both (and `--help` text in the clap
definition) — they are kept in sync by hand.

Quick smoke test: `cargo build --release && ./target/release/map_to_image maps/forest_sample.omap`

Rendering text correctly wants the system's `fontconfig` library (see
[README's Fonts and fontconfig section](README.md#fonts-and-fontconfig)) — it's
loaded by name at runtime (`dlopen`), never linked, so it's not needed to build.

## Architecture

Everything is built on one data model: [`map.rs`](src/map.rs) is what a map file
says. [`xml_reader.rs`](src/xml_reader.rs) parses a file into it,
[`xml_writer.rs`](src/xml_writer.rs) writes one back out, and every other module is
a *consumer* of that model — a new thing to do with a map is a new module, not a new
parser. [`ocd.rs`](src/ocd.rs) reads OCAD's binary `.ocd` format by converting it to
the same `map.rs` model (`ocd_crs.rs` maps its coordinate-system number to an
EPSG code). Start reading at **`src/lib.rs`**'s own crate-level doc
comment — it is a short, current map of how every module fits together and is
worth reading in full before making cross-cutting changes.

There are two largely independent pipelines living in this one crate:

### 1. Rendering pipeline (`map_to_image`, `benchmark`)

`render::render_map` is the whole thing in one call: read the file, build
renderables, rasterize.

- **`renderer.rs`** turns map objects into an internal `Path` IR (draw order,
  dash groups, mid symbols, area fill patterns) — kept backend-independent so
  `geometry.rs` doesn't depend on `tiny-skia`; conversion to `tiny_skia::Path`
  happens only at paint time.
- **`geometry.rs`** / **`qbezier.rs`** are the exact-arithmetic layer: path
  flattening, offsetting, tangents. `qbezier.rs` is Qt's own bezier-offset
  algorithm, ported to keep border rendering pixel-faithful to Mapper.
- **`text.rs`** is font loading/shaping/layout (`fontdb` + `rustybuzz` +
  `ttf-parser`, the same stack `resvg` uses), laid out large and scaled down.
- **`render.rs`** is the map-file-to-pixel-buffer glue shared by `map_to_image`
  and `benchmark`, so both draw a map through identical code.

**Benchmarking** (`create_benchmark` → `benchmark`) exists because "close enough"
can't be judged in the abstract: a suite is scored against an external ground-truth
OMap renderer (see `create_benchmark`'s `<renderer>` arg). Key pieces:
`all_symbols.rs` (one generated test map per symbol, laid out by running the
renderer over each object to measure it), `naming.rs` (the ordinal naming rules a
benchmark archive must follow, and how to repair one that doesn't), `differences.rs`
(the pixel diff + the antialiasing-vs-real classification — see
[ImplementationDetails.md § Antialiasing](mds/ImplementationDetails.md#antialiasing)
for *why* that split exists and how it's computed). Full details, including the
exact on-disk archive/results layout, are in **`mds/ImplementationDetails.md`**.

### 2. Contour-to-elevation pipeline (`contours_to_raster`)

A five-step algorithm, specified in full (including all math/force-model
parameters) in **`mds/Contours-to-Raster.md`** — read that doc before touching
this pipeline; the source is a fairly literal implementation of it and the doc
names the concepts the code assumes you already know (Flying Ends, Rain Drop
Productions, the Contour Raster's reserved pixel values, etc). One source file per
step, named to match:

- `contour_symbols.rs` — classifies a map's symbols into the 4 families the
  algorithm cares about (Contours, Slope Lines, Jumps, Heavy Objects).
- `contour_geometry.rs` — Appendices 1 & 2 of the doc (bezier→LineString, LineString→buffered polygon).
- `gravity_model.rs` — Step 1's data model (per-contour downhill-direction evidence).
- `contour_raster.rs` — the Contour Raster itself (Step 1) + the Appendix 4 pixel-walk used by Steps 1 and 3.
- `step1_extract.rs` → `step2_obvious_gravity.rs` → `step3_rain_drop.rs` (also the shared Rain Drop Production engine) → `step4_elevation.rs` → `step5_elevation_raster.rs` / `step5_gravity_raster.rs` — the steps in order.
- `contours_to_raster_config.rs` — the ~35 tunable parameters, read from `config/contours_to_raster.conf` (a hand-rolled `key = value` format; no serde/toml dependency in this crate for one 35-line file).
- `dem_from_contours.rs` — Steps 1–5 in memory on an already-parsed `Map`, with a progress callback and no file I/O: the library entry point (used by the wasm build) parallel to the binary.
- `contours_to_raster_svg.rs` — the `--create_svg` diagnostic dumps (one SVG per sub-stage, see the doc's "Visualization" section).
- `geotiff.rs` — georeferencing (affine transform + GeoTIFF tags) for the final elevation raster.

`dem.rs` (terrain shading for display) and `dem_geo_tiff_evaluator.rs` (scoring a
predicted DEM against ground truth, used by the `dem_geo_tiff_evaluator` binary)
are related but downstream of this pipeline's output, not part of it.

### Other consumers of `map.rs`

- **`course.rs`** — lays a course (start triangle, control circles, connecting lines, numbers) over a map.
- **`route.rs`** + **`runnability.rs`** — `runnability` turns a map into a per-cell running-speed grid; `route` searches it for the fastest path between two points.
- **`snap.rs`** — what a control point can be placed on (proximity to point features).
- **`stats.rs`** — counts of a map's objects/symbols/lengths/areas.
- **`svg.rs`** — writes the renderer's own renderables out as SVG vector paths instead of rasterizing them.
- **`validate.rs`** — checks a map against a mapping standard (ISOM/ISSprOM).

### OpenStreetMap → map (`osm_to_omap`)

The one pipeline that *produces* a map rather than consuming one: `osm.rs`
parses `.osm` XML, `osm_style.rs` is the openstreetmap-carto symbol set and
tag→symbol rules, and `osm_convert.rs` projects to UTM, clips, and writes the
map through `xml_writer.rs`. Optional overlays: `bus.rs` (bus/train lines from
OSM route relations or GTFS), `bundle.rs` (lines sharing a street laid out
side by side), `map_areas.rs` (event map areas from a CSV).

### Orthophotos (`add_orthophoto`)

`orthophoto.rs` says *what* to download and never touches the network: the
regional WMS services (only Emilia-Romagna's AGEA flights so far), a map's
footprint in its projected CRS, the tile grid, `GetMap` URLs (WMS 1.1.1, so
the bbox is always easting first), capabilities parsing, the world file.
`image_template.rs` adds the image to the map file as a `TemplateImage` by
editing the file's *text*, not by round-tripping through `map.rs`, which
would lose everything Mapper keeps that the model doesn't. The binary does
the HTTP itself (`ureq`, a native-only dependency, so o-mnia's wasm build
never sees it). Its integration tests (`tests/add_orthophoto.rs`) cover only
the offline exit paths, so CI never depends on the region's server.

### Benchmark support modules

`archive_info.rs` (reads an archive's `info.txt` back), `progress.rs`
(tqdm-style progress bar), `report.rs` (word wrapping for the `.txt` reports).

## Testing conventions

- Integration tests (`tests/*.rs`) drive the **built binaries** via `assert_cmd`,
  not the library directly, where the CLI's exit codes and messages are the
  documented contract being tested (see `tests/cli.rs`'s own doc comment).
- Unit tests for pure-arithmetic modules (`geometry.rs`, `qbezier.rs`) check
  specific geometric invariants (extents, offset direction, slice round-trips),
  not just "it compiles."
- Shared fixtures live in `tests/data/` (`*.xmap` maps, `bologna_sample.osm`, a `gtfs_sample/` feed).
- `tests/regressions.rs` pins inputs that once broke something, each as a minimal hand-written map.
- `maps/` holds sample maps used in the README and manual smoke-testing.
  `benchmarks/`, `Results/`, `dataset/`, `no_public_maps/`, `trainings/` are all
  gitignored — local/generated data, not tracked in the repo.

## Other docs worth knowing about

- **`mds/ImplementationDetails.md`** — crate/dependency stack, full source-layout
  table, benchmark archive format, antialiasing classification.
- **`mds/Contours-to-Raster.md`** — the full spec for the `contours_to_raster`
  pipeline (algorithm steps, appendices, every config parameter's meaning).
  **`mds/Contours-to-Raster-Overview.md`** is a short, implementation-free,
  mathematically-formal account of the same algorithm.
- **`mds/bug.md`** — known rendering discrepancies against the ground-truth
  renderer, with pictures.
