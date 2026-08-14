# Benchmarks

Real orienteering maps, rendered and compared against stored reference images.
Neither the maps nor the images are part of the repository — they are hundreds
of megabytes, and the maps are not ours to distribute — so this directory
would be empty in a fresh checkout of just this crate; it is populated here by
copying `maps/` and `expected/` straight from the parent C++ project's own
`../../benchmarks/`, which already went through the setup below.

```
benchmarks/
    maps/         NN_Name.omap, the input maps        (git-ignored)
    expected/     NN_Name.png, the reference images    (git-ignored)
    predictions/  NN_Name.png, what ./benchmarks.sh renders  (git-ignored)
    differences/  NN_Name/, where the two disagree      (git-ignored)
```

Maps are named with a two digit ordinal, an underscore, and the map name with
spaces replaced by underscores, so that names are stable and free of quoting
problems.

The reference images in `expected/` were rendered by **OpenOrienteering
Mapper** itself (via the C++ `map_to_image` one directory up), so a failure
here means this Rust renderer draws a map differently from Mapper — the same
fidelity signal the C++ project's own benchmarks track. See `../README.md`'s
"Fidelity" section for the current pass rate and known gaps.


## Setting them up

`setup.sh` and `regenerate.sh` are carried over unchanged from the C++
project (`../../benchmarks/`) — they only need a `map_to_image` executable
path, and know nothing about how it was built:

```bash
cargo build --release
benchmarks/setup.sh target/release/map_to_image ~/maps/OMOLOGATE
```

It searches the collection recursively for `.omap` files, copies them into
`maps/` under the naming convention above, and renders every one of them into
`expected/` with the given tool. The collection itself is left untouched, and
`setup.sh` refuses to overwrite an existing `maps/`/`expected/` unless passed
`--force`, since renumbering shifts every test name.

To add or re-render maps without redoing the whole collection, edit `maps/`
by hand and use `regenerate.sh`, which only does the rendering half and
defaults to `../target/release/map_to_image`.

In this project maps/expected are normally just kept in sync with the
sibling C++ `benchmarks/` folder instead of re-running `setup.sh` from
scratch — both trees should reference the same underlying map collection.


## Running them

```bash
./benchmarks.sh              # the whole suite
./benchmarks.sh 501          # only maps whose name contains "501"
./benchmarks.sh --tolerance=60 --threshold=0.01
```

Unlike the C++ project's ctest-per-map setup, `../benchmarks.sh` renders and
compares the whole suite in one pass (it's meant to be run often while
iterating) and prints a summary:

```
== 164 / 169 passed (tolerance=60, threshold=0.01) ==
Worst: 04_004_text_102.1_Contour_value (100% beyond tolerance)
Failed (5): ...
```

`predictions/` is git-ignored and outlives any build, so a failing benchmark
can be looked at right next to its reference image:

```bash
xdg-open benchmarks/predictions/NAME.png benchmarks/expected/NAME.png
```


## Looking at the differences

`differences.py`, also carried over unchanged from the C++ project, turns
`predictions/` and `expected/` into something to look at:

```bash
benchmarks/differences.py
```

It compares the two image by image, prints a line per differing pair, and
writes a subfolder of `differences/` for each of them:

```
differences/NAME/
    side_by_side.png       both images whole, expected left, predicted right
    diff.png               black where they agree, red where they do not
    crop_001_2888x1352.png the worst region: expected, predicted and diff
    crop_002_2856x1432.png the second worst region, and so on
```

There are as many crops as it takes to cover **every** difference, worst
region first. Useful options: `--tolerance` ignores small differences the
same way `image_compare` does; `--filter TEXT` limits the run to maps whose
name contains `TEXT`; `--crops`/`--crop-size` control how much is cropped
out. Needs `numpy` and `Pillow` (`pip install numpy pillow`).


## Accepting a rendering change

```bash
benchmarks/regenerate.sh
```

This overwrites every reference image with this Rust tool's own output,
turning the benchmark from a fidelity test (against Mapper) into a
regression test (against itself) — only do this when a rendering change is
understood and intended, and prefer re-copying `expected/` from the C++
project's benchmarks instead if the goal is to compare against Mapper again.
