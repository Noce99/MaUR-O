# Known Rendering Differences

Where MaUR-O's output differs from the ground-truth renderer, sorted into
real bugs, differences not worth fixing, and cases where MaUR-O is arguably
the more correct of the two.

- [Tested benchmarks](#tested-benchmarks)
- [ISOM/ISSprOM per-symbol benchmark](#isomissprom-per-symbol-benchmark)
- [Real map dataset](#real-map-dataset)

## Tested benchmarks

Three benchmarks are public and reproducible from what's in this repository
(a fourth, over a private real map dataset, is not — see
[Real map dataset](#real-map-dataset) below). All three follow the same
pattern: build the archive with `create_benchmark`, then run it with
`benchmark`. See [README.md](../README.md#usage) for what the options mean.

**The two sample maps**, rendered whole rather than split by symbol:

```bash
./target/release/create_benchmark /path/to/ground_truth_renderer maps/ --filter sample -o benchmarks/benchmark_sample_maps_3_px_m.zip
./target/release/create_benchmark /path/to/ground_truth_renderer maps/ --filter sample -r 6 -o benchmarks/benchmark_sample_maps_6_px_m.zip
./target/release/benchmark benchmarks/benchmark_sample_maps_3_px_m.zip
./target/release/benchmark benchmarks/benchmark_sample_maps_6_px_m.zip
```

**ISOM 2017-2 (`maps/ISOM_10k.omap`, a 1:10000 map), split one map per
symbol:**

```bash
./target/release/create_benchmark /path/to/ground_truth_renderer maps/ISOM_10k.omap
./target/release/create_benchmark /path/to/ground_truth_renderer maps/ISOM_10k.omap -r 6
./target/release/benchmark benchmarks/benchmark_ISOM_10k_3_px_m.zip
./target/release/benchmark benchmarks/benchmark_ISOM_10k_6_px_m.zip
```

**ISSprOM 2019 (`maps/ISSprOM_4k.omap`, a 1:4000 sprint map), split one map
per symbol:**

```bash
./target/release/create_benchmark /path/to/ground_truth_renderer maps/ISSprOM_4k.omap
./target/release/create_benchmark /path/to/ground_truth_renderer maps/ISSprOM_4k.omap -r 6
./target/release/benchmark benchmarks/benchmark_ISSprOM_4k_3_px_m.zip
./target/release/benchmark benchmarks/benchmark_ISSprOM_4k_6_px_m.zip
```

`/path/to/ground_truth_renderer` is the external ground-truth renderer (not
MaUR-O's own `map_to_image`) — see
[Ground-truth renderer](../README.md#ground-truth-renderer) for what that needs to be.
Both resolutions (3 and 6 pixels per meter) were run over each suite; the
images below are picked from whichever run happened to show them, at
either resolution.

## Differences found

### ISOM/ISSprOM per-symbol benchmark

#### Bugs

![ISOM area 113 Broken ground: an extra pattern dot rendered in one cell of the fill grid](assets/bug_isom_1.png)
*ISOM per-symbol benchmark — area 113 Broken ground, one cell of the rotated
fill-pattern grid: one extra dot is rendered, the two empty orange
circles nearby are ordinary antialiasing.*

![ISOM area 402 Open land with scattered trees: a seam of wrong pixels along a fill boundary](assets/bug_isom_2.png)
*ISOM per-symbol benchmark — area 402 Open land with scattered trees: an extra yellow 1 px thick line is rendered.*

![ISOM area 406.1 Vegetation, slow running / normal running in one direction: fill edge off by a few pixels](assets/bug_isom_3.png)
*ISOM per-symbol benchmark — area 406.1 Vegetation (slow running / normal
running in one direction): an extra green 1 px thick line is rendered.*

#### Non Bugs

Differences we don't think are worth caring about — extreme corner cases
that don't come up on a real map.

![ISOM major power line: a single stray pixel where a mast tick meets a tightly curved line](assets/non_bug_isom_1.png)
*ISOM per-symbol benchmark — line 511.1 / combined 511.2 Major power line
(with large carrying masts): a shorter line is rendered in an extreme corner case.*

![ISOM major power line at its minimum length: a textured ring in the ground truth versus a solid dot in MaUR-O](assets/non_bug_isom_2.png)
*ISOM per-symbol benchmark — line 511 Major power line, at its minimum
length: different behaviour when one of the two parts of a power line is collapsing in a circle.*

![ISOM railway at its minimum length: the same textured-ring-versus-solid-dot difference](assets/non_bug_isom_3.png)
*ISOM per-symbol benchmark — combined 509 Railway, at its minimum length: different behaviour when one of the symbol's borders collapses on itself*

#### Improvements

Cases where MaUR-O's own rendering looks more correct than the ground
truth's, in our opinion.

![ISOM wide road at a tight bend: the ground truth leaves an uncoloured gap between the road's border and fill that MaUR-O does not](assets/improvement_isom_1.png)
*ISOM per-symbol benchmark — line 502 Wide road, minimum length case, at a
tight bend: OMapper inserts a non coloured space between the road border and the road itself, MaUR-O does not. This is not a corner case but a well-known OMapper issue.*

![ISOM narrow ride center dot: the ground truth leaves an unfilled diamond hole that MaUR-O fills solid](assets/improvement_isom_2.png)
*ISOM per-symbol benchmark — combined 508.1 Narrow ride, easy running,
center dot: the ground truth leaves a small unfilled white diamond inside
the black center dot; MaUR-O fills it solid.*

![ISOM road with two carriageways: the ground truth leaves an unfilled ring that MaUR-O fills solid](assets/improvement_isom_3.png)
*ISOM per-symbol benchmark — combined 502.2 Road with two carriageways,
rotation grid cell: the ground truth leaves a thin unfilled white ring
inside the fill; MaUR-O fills it solid.*

![ISOM large building with outline: a stray white speck in the ground truth's fill that MaUR-O doesn't reproduce](assets/improvement_isom_4.png)
*ISOM per-symbol benchmark — combined 521.2 Large building with outline:
the ground truth leaves a stray one-pixel white speck inside the solid
fill; MaUR-O's fill has none.*

### Real map dataset

Found running the benchmark over a private dataset of real
orienteering maps (not included in this repository).

![The edge of a purple symbol is missing](assets/bug_1.png)
*Real map dataset: the edge of a purple symbol is missing.*

![An extra black dot of a stony area, drawn by MaUR-O but not by the ground truth](assets/bug_2.png)
*Real map dataset: an extra black dot of a stony area is drawn.*

![Where a black line turns back on itself, MaUR-O draws the turn too short](assets/bug_3.png)
*Real map dataset: where a line turns back on itself (a ~180° turn), the turn is drawn too short.*

![The end of a green vegetation-boundary dash at a sharp turn is drawn shorter than in the ground truth](assets/bug_4.png)
*Real map dataset: the same too-short turn, on a green vegetation boundary.*

![The end of a black dash is drawn with a hook instead of a straight cut](assets/bug_5.png)
*Real map dataset: the end of a black dash is drawn with a small hook instead
of a straight cut.*

![The start of a blue water-channel dash is misshapen next to a contour crossing](assets/bug_6.png)
*Real map dataset: the start of a dash of a seasonal water channel is misshapen.*
