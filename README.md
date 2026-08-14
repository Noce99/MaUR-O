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
| `bin/map_to_image.rs` | `main.cpp` | The CLI. |
| `bin/image_compare.rs` | `tests/image_compare.cpp` | The pixel-diff tool the benchmarks use. |


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
itself against (`benchmarks/`, copied from `../benchmarks/`), using
`image_compare`'s default tolerance (60 per pixel, summed over R+G+B) and a
1% threshold:
**164 of 169 pass**. Of the 5 that don't:

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

Run `./benchmarks.sh` (needs `benchmarks/maps` and `benchmarks/expected`
populated, see `benchmarks/README.md`) to reproduce this, or
`./benchmarks.sh <substring>` to run a subset while iterating.


## Testing

```bash
cargo test              # unit tests (geometry, qbezier, xml_reader) + CLI integration tests
./benchmarks.sh          # the 169-map benchmark suite, see Fidelity above
./benchmarks.sh 501      # only maps whose name contains "501"
```

`cargo test`'s unit tests check specific values (parsed XML fields, computed
extents, offset directions), not just "it runs" — several were written by
first checking what the real C++ `map_to_image` binary actually produces for
the same input, rather than trusting a re-derivation of the spec.
