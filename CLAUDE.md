# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

MaUR-O (**Ma**p **U**tils in **R**ust for **O**rienteering) renders OpenOrienteering
Mapper `.omap`/`.xmap` files (and OCAD `.ocd` files, converted to Mapper's format on
the way in) to raster images in pure Rust — no Qt, no Mapper, no graphical
environment. Alongside the renderer it ships benchmarking tools that compare its
output against a ground-truth renderer, and a second, independent pipeline
(`mauro dem`) that extracts elevation/gravity information from a map's
contour lines. `mauro osm` goes the other way: it turns
OpenStreetMap data (plus optional GTFS transit feeds) into a Mapper map, and
`mauro ortho` downloads the aerial orthophoto of a map's ground
(from about fifty open national and regional WMS services across Europe)
and adds it to the map as a template. `mauro net` is the machine-learning side:
it generates a dataset of random maps with pixel labels, trains a U-Net (`burn`)
to read a rendered map back into its opaque area symbols, and turns what it
reads into a map again (`src/net/`, `vectorize.rs`; the backend is a Cargo
feature, `flex` by default, `wgpu` or `cuda` for a GPU). All of these are
subcommands of the one binary, `mauro`.

This crate is consumed by the o-mnia web app from a vendored copy (via
`crates/maur-o-wasm`, o-mnia's own wasm-bindgen adapter — see that repo's
`docs/dev/rust-backends.md`). Keep this crate a general-purpose **algorithms
library plus its own CLI tools** — no UI in the library (the `mauro`
binary's terminal interface, `src/bin/mauro/tui/`, is part of the CLI tools,
and only ever builds and runs `mauro` command lines), no o-mnia app policy (Settings,
profiles, project files, extension contracts, worker/store plumbing). A new
capability an app needs is new library/CLI surface here; the app-facing
wiring (wasm export, job protocol, UI) belongs in o-mnia, not here.

## Commands

```bash
cargo build --release                          # build everything; the binary is target/release/mauro
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
MSRV check pinned to Rust 1.95 (the floor set by `burn`). There is no
Cargo workspace — one crate, one binary (`mauro`) with a subcommand per tool.

### The `mauro` subcommands

| Subcommand | Purpose |
| --- | --- |
| `mauro render` | Render a single map to PNG/BMP/TIFF/JPEG, or to SVG. |
| `mauro benchmark create` | Build a benchmark archive (maps + ground-truth reference images) from a folder of maps or a single map (in which case one test map per symbol is generated). |
| `mauro benchmark run` | Run a benchmark archive: render every map, diff against the reference, write a report under `Results/`. |
| `mauro dem` | Run the 5-step elevation-extraction pipeline (see below) on one map, writing an elevation GeoTIFF. |
| `mauro dem-eval` | Score a predicted DEM GeoTIFF against a ground-truth one (MSE/RMSE/MAE + error maps). |
| `mauro osm` | Convert OpenStreetMap XML (a file, or downloaded for an area from the Overpass API with `--download`) into an `.omap` drawn to ISSprOM 2019 (default) or ISOM 2017-2, or (`--standard osm`, optionally with GTFS feeds) styled like openstreetmap-carto. |
| `mauro ortho` | Download a map's aerial orthophoto from the best open WMS for its place (European national/regional agencies) and add it to the map as a georeferenced template. |
| `mauro net dataset` | Generate a dataset of random maps out of one map's symbol set: maps, images and per-pixel labels (`images/`, `gt/`, `classes.json`). `--min-resolution/--max-resolution` draws each map at its own density (same pixels, more or less ground; `resolutions.csv`), which gives the U-Net a scale head. |
| `mauro net grid-to-map` | Turn a label grid back into a map (the way back from `gt/`). |
| `mauro net train` | Train a network on such a dataset: a U-Net (`--model unet`, the default) or a tree of curves (`--model tree`, `src/net/tree.rs`, `tree_training.rs`). `--dashboard` for burn's live terminal dashboard (U-Net only); the TUI's job screen replaces it. |
| `mauro net image-to-map` | Read a picture of a map back into a map with a trained run, and score it by drawing it again. The picture's density comes from `--resolution`, `--dpi`, the scale head or `classes.json`, and a picture outside the densities the run was trained over is resampled to the nearer end of them before it is read. |
| `mauro net show-batches` | Draw the batches a run would be trained on, from the tensors, to check the labels. |
| `mauro net describe` | Write each network's architecture (`net/describe.rs`) into the versioned `networks/` folder; `--check` is the CI/hook gate. |

`src/bin/mauro/main.rs` is only the dispatcher (the clap `Cli`, one module per
subcommand, each module's `Args` and `pub fn main(args) -> ExitCode`). A usage
error is exit code 1 for every subcommand. A bare `mauro` in a terminal opens
the ratatui interface in `src/bin/mauro/tui/` (elsewhere it prints help, exit
1). Its rule: a screen never runs a tool itself — it builds the subcommand's
argument list, checks it with `tui::check` (the same clap `Cli`), and runs
`mauro` with it as a child process (`tui/job.rs`, which sets
`progress::LIVE_ENV` so progress bars still draw through the pipe) — so it
can't drift from the CLI. The one exception to "runs and waits" is a long job
(`mauro net train`, a `Tool::background()` kind): `tui/jobs.rs` starts it
detached (own process group, output to a file) with `MAURO_JOB_DIR` set, and
it reports through that folder (`net/monitor.rs`: `status`, `losses`, and a
`control` file for pause/stop). The home screen's jobs panel lists those
folders and `tui/watch.rs` draws one, so closing the TUI never touches the job. Each screen (`tui/screens/<subcommand>.rs`) is a
`form::Tool` on the shared `form::Form`; a field naming its clap argument
(`Field::arg("id")`) takes its help and default from clap, so don't copy those
into the TUI. Defaults are not in the clap definitions: every option's default is in
`config/defaults.toml` (a table per subcommand, a key per arg id), applied to
the parser at run time by `src/bin/mauro/defaults.rs` — always build the
parser with `crate::defaults::command()`, never `Cli::command()`/`Cli::parse`,
or the defaults are missing and the args they make optional are required.
A new option with a default gets a key there (`every_default_is_an_argument_and_parses`
checks the keys), and the TUI's Ctrl+D writes that file. Adding or renaming a flag: update that subcommand's screen too —
a stale arg id panics `Form::new`, caught by `every_tool_opens_and_draws_its_screen`,
and a flag a screen still emits fails that screen's unit tests. Each
subcommand module also has `exit_meaning(code)`, mirroring its doc comment's
exit codes, which the job screen shows; keep the two in sync. Full CLI
flags for each are documented in its module's own top-of-file doc comment
(`src/bin/mauro/*.rs`) and in `README.md`'s Usage section. When changing a
flag, default or exit code, update both (and `--help` text in the clap
definition) — they are kept in sync by hand.

Quick smoke test: `cargo build --release && ./target/release/mauro render maps/forest_sample.omap`

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

### 1. Rendering pipeline (`mauro render`, `mauro benchmark run`)

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
- **`render.rs`** is the map-file-to-pixel-buffer glue shared by `mauro render`
  and `mauro benchmark run`, so both draw a map through identical code.

**Benchmarking** (`mauro benchmark create` → `mauro benchmark run`) exists because "close enough"
can't be judged in the abstract: a suite is scored against an external ground-truth
OMap renderer (see `mauro benchmark create`'s `<renderer>` arg). Key pieces:
`all_symbols.rs` (one generated test map per symbol, laid out by running the
renderer over each object to measure it), `naming.rs` (the ordinal naming rules a
benchmark archive must follow, and how to repair one that doesn't), `differences.rs`
(the pixel diff + the antialiasing-vs-real classification — see
[ImplementationDetails.md § Antialiasing](mds/ImplementationDetails.md#antialiasing)
for *why* that split exists and how it's computed). Full details, including the
exact on-disk archive/results layout, are in **`mds/ImplementationDetails.md`**.

### 2. Contour-to-elevation pipeline (`mauro dem`)

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
- `dem_from_contours.rs` — Steps 1–5 in memory on an already-parsed `Map`, with a progress callback and no file I/O: the library entry point (used by the wasm build) parallel to `mauro dem`.
- `contours_to_raster_svg.rs` — the `--create_svg` diagnostic dumps (one SVG per sub-stage, see the doc's "Visualization" section).
- `geotiff.rs` — georeferencing (affine transform + GeoTIFF tags) for the final elevation raster.

`dem.rs` (terrain shading for display) and `dem_geo_tiff_evaluator.rs` (scoring a
predicted DEM against ground truth, used by `mauro dem-eval`)
are related but downstream of this pipeline's output, not part of it.

### Other consumers of `map.rs`

- **`course.rs`** — lays a course (start triangle, control circles, connecting lines, numbers) over a map.
- **`route.rs`** + **`runnability.rs`** — `runnability` turns a map into a per-cell running-speed grid; `route` searches it for the fastest path between two points.
- **`snap.rs`** — what a control point can be placed on (proximity to point features).
- **`stats.rs`** — counts of a map's objects/symbols/lengths/areas.
- **`svg.rs`** — writes the renderer's own renderables out as SVG vector paths instead of rasterizing them.
- **`validate.rs`** — checks a map against a mapping standard (ISOM/ISSprOM).
- **`tree_vector.rs`** — a tree-network answer (cuts + a symbol per leaf) as polygons, by clipping with the cuts (`geo` boolean ops); `vectorize::objects_of_regions` writes them as map objects. The network's side-of-a-cut definition (`net::tree::signed_distance`) must stay the polygon parity this uses, or the export stops matching what was trained.

### OpenStreetMap → map (`mauro osm`)

The one pipeline that *produces* a map rather than consuming one: `osm.rs`
parses `.osm` XML; the symbols come from an `OsmRules` (in `osm_convert.rs`):
`osm_orienteering.rs` (`--standard issprom|isom`, the default) is a standard's
symbol set, copied verbatim from `maps/ISSprOM_4k.omap` / `maps/ISOM_10k.omap`,
plus tag→code rules; `osm_style.rs` (`--standard osm`) is the
openstreetmap-carto symbol set and tag→symbol rules. `osm_convert.rs`
projects to UTM, clips, cuts land cover largest-first where the standard's
colour order would hide it, and writes the map through `xml_writer.rs`.
Optional overlays, `--standard osm` only: `bus.rs` (bus/train lines from
OSM route relations or GTFS), `bundle.rs` (lines sharing a street laid out
side by side), `map_areas.rs` (event map areas from a CSV).
`osm_download.rs` says what to ask the Overpass API for an area (the query,
reading its error remarks) and never touches the network; the binary does
the HTTP (`ureq`, native-only, as in `mauro ortho`), and its integration
tests cover only the offline paths.

### Orthophotos (`mauro ortho`)

`orthophoto.rs` says *what* to download and never touches the network: a
map's place on the earth (`ref_point_deg`), the CRS to ask in (the map's
own, else EPSG:3857, else 4326 -- the footprint in those is laid out from
the reference point on the ellipsoid, and Mapper reprojects the image), the
tile grid, `GetMap` URLs (WMS 1.1.1, or 1.3.0 where a server speaks nothing
else), capabilities parsing, the world file, and what counts as "no photo"
in a coverage probe. `orthophoto_services.rs` is the hand-checked list of
services, in priority order (city before region before country). The binary
asks each service whose box holds the map for a small transparent probe and
takes the first with photos of all of it. `image_template.rs` adds the image
to the map file as a `TemplateImage` by editing the file's *text*, not by
round-tripping through `map.rs`, which would lose everything Mapper keeps
that the model doesn't. The binary does the HTTP itself (`ureq`, a
native-only dependency, so o-mnia's wasm build never sees it). Its
integration tests (`tests/add_orthophoto.rs`) cover only the offline exit
paths, so CI never depends on anyone's server.

### Benchmark support modules

`archive_info.rs` (reads an archive's `info.txt` back), `progress.rs`
(tqdm-style progress bar), `report.rs` (word wrapping for the `.txt` reports).

### Network descriptors (`networks/`)

A network's architecture is *declared* beside its `forward` (`UNetConfig::describe`,
`TreeNetConfig::describe`, built with `net/describe.rs`'s `Node`/`edge` builder)
and registered as a versioned JSON in `networks/<scope>/<net>/<version>.json`
by `mauro net describe` (`net/registry.rs`); the o-mnia-independent MauNeVi
visualizer reads that folder and nothing else. **Changing a network's layers,
widths or tensor shapes means updating its `describe` and running
`mauro net describe`, committing `networks/` with the change** — CI, the
`tests/net_describe.rs` test and the optional `.githooks/pre-commit` all run
`--check`. A `forward_tapped` reports every tensor's shape under the name its
edge has, and each network's `the_descriptor_matches_the_forward_pass` test
holds the declaration to it; a new layer in a `forward` needs a tap and an edge.
The hash that decides "new version" covers the architecture only (not the date
or commit), and registered files are never rewritten.

## Testing conventions

- Integration tests (`tests/*.rs`) drive the **built `mauro` binary** via `assert_cmd`,
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
- **`mds/Contours-to-Raster.md`** — the full spec for the `mauro dem`
  pipeline (algorithm steps, appendices, every config parameter's meaning).
  **`mds/Contours-to-Raster-Overview.md`** is a short, implementation-free,
  mathematically-formal account of the same algorithm.
- **`mds/bug.md`** — known rendering discrepancies against the ground-truth
  renderer, with pictures.
