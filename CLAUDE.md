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
and adds it to the map as a template. All of these are subcommands of the one binary,
`mauro`. `mauro tiles build|query|serve` are the
command line of MOT, a draft tiled format for keeping all the world's maps as
one map (`mds/MOT-Format.md`).

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
MSRV check pinned to Rust 1.88 (the floor set by the `image` crate). There is no
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
| `mauro tiles build` | Convert maps into a MOT archive: the tiled, compact world-map format of `mds/MOT-Format.md` (draft), in PMTiles. |
| `mauro tiles query` | List the objects of a MOT archive around a place (`--around`/`--bbox`), by symbol, reading only the tiles it touches. |
| `mauro tiles serve` | Serve a MOT archive as a slippy map to look at in a browser (a std-only HTTP server, one HTML page embedded in the binary); tiles are drawn server-side by the crate's renderer. A development viewer, not o-mnia's. |

`src/bin/mauro/main.rs` is only the dispatcher (the clap `Cli`, one module per
subcommand, each module's `Args` and `pub fn main(args) -> ExitCode`). A usage
error is exit code 1 for every subcommand. A bare `mauro` in a terminal opens
the ratatui interface in `src/bin/mauro/tui/` (elsewhere it prints help, exit
1). Its rule: a screen never runs a tool itself — it builds the subcommand's
argument list, checks it with `tui::check` (the same clap `Cli`), and runs
`mauro` with it as a child process (`tui/job.rs`, which sets
`progress::LIVE_ENV` so progress bars still draw through the pipe) — so it
can't drift from the CLI. Each screen (`tui/screens/<subcommand>.rs`) is a
`form::Tool` on the shared `form::Form`; a field naming its clap argument
(`Field::arg("id")`) takes its help and default from clap, so don't copy those
into the TUI. Adding or renaming a flag: update that subcommand's screen too —
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
services, in priority order (city before region before country). Google's
satellite tiles (`orthophoto_services::GOOGLE`, a `TileService`: XYZ tiles
in EPSG:3857, `TileRange` turning a box into tiles and a `Grid`) are
separate from that list, used only by `--service google`, never picked
automatically. The binary
asks each service whose box holds the map for a small transparent probe and
takes the first with photos of all of it. `image_template.rs` adds the image
to the map file as a `TemplateImage` by editing the file's *text*, not by
round-tripping through `map.rs`, which would lose everything Mapper keeps
that the model doesn't. The binary does the HTTP itself (`ureq`, a
native-only dependency, so o-mnia's wasm build never sees it). Its
integration tests (`tests/add_orthophoto.rs`) cover only the offline exit
paths, so CI never depends on anyone's server.

### MOT, the tiled world map (`mauro tiles build`, `mauro tiles query`)

Design and decisions: `mds/MOT-Format.md` (a draft). `mot.rs` is the pure part:
Web Mercator tile addressing, the integer grid, and the tile payload codec.
`mot_symbols.rs` is a layer's symbol table (ISOM 2017-2 or ISSprOM 2019, and which
of them a map is drawn to), the
closest-symbol rule and each symbol's zoom rule (when it appears, how small it
may be, how much its line may be thinned); `mot_convert.rs` places a `Map` on the earth and stores each
object in the tiles it touches, cut to each tile's box plus a buffer, and the
area each map covers (`paper_extent`: a box in the map's frame, stored as an object
of `COVERAGE_SYMBOL` in every level, which is all the levels below the first
symbol have)
(`mot_clip.rs`: curves flattened, lines into pieces, areas
into rings with their cut edges flagged); `mot_history.rs` is the pure part of versioning (changesets, a tile's versions, undo
as a new change; the head stays a plain tile payload). `convert_levels` builds the pyramid from the finest level up to a coarser one,
generalising by those rules; `mot_archive.rs` keeps the
tiles in a PMTiles file through the `pmtiles` crate (native only, like `ureq`,
so the wasm build never sees it). `mot_render.rs` draws a tile by turning it back
into a `Map` (the ISOM symbols, paper mm at a scale the viewer picks) and handing
it to the same `Renderer` as `.omap` files; `mauro tiles serve` puts that behind a
small HTTP server and a one-page viewer, with the layers (one archive per symbol set)
stacked by priority and masked by each one's coverage (`composite`) as a way to look at an archive. The
real viewer is o-mnia's (a wasm build over this crate), not this binary. Source of truth, logins, servers and the UI
are o-mnia's, not this crate's; the format only defines tiles and what to do
with them.

### Benchmark support modules

`archive_info.rs` (reads an archive's `info.txt` back), `progress.rs`
(tqdm-style progress bar), `report.rs` (word wrapping for the `.txt` reports).

## Testing conventions

- Integration tests (`tests/*.rs`) drive the **built `mauro` binary** via `assert_cmd`,
  not the library directly, where the CLI's exit codes and messages are the
  documented contract being tested (see `tests/cli.rs`'s own doc comment).
- Unit tests for pure-arithmetic modules (`geometry.rs`, `qbezier.rs`) check
  specific geometric invariants (extents, offset direction, slice round-trips),
  not just "it compiles."
- The MOT tests (`tests/mot.rs`, `tests/tiles_serve.rs`) share `tests/common/mod.rs`, which makes a small
  map with `osm_to_omap` and archives from it, so no file of ours holds one.
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
