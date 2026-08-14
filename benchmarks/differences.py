#!/usr/bin/env python3
#
#    Copyright 2026 Enrico Mannocci
#
#    This file is part of map_to_image, derived from OpenOrienteering Mapper.
#
#    This program is free software: you can redistribute it and/or modify
#    it under the terms of the GNU General Public License as published by
#    the Free Software Foundation, either version 3 of the License, or
#    (at your option) any later version.
#
#    This program is distributed in the hope that it will be useful,
#    but WITHOUT ANY WARRANTY; without even the implied warranty of
#    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
#    GNU General Public License for more details.
#
#    You should have received a copy of the GNU General Public License
#    along with this program.  If not, see <http://www.gnu.org/licenses/>.
#
"""Show where the benchmark renderings differ from their reference images.

Compares benchmarks/expected/ against benchmarks/predictions/ image by image.
Identical pairs are reported and skipped; for every differing pair a subfolder
of benchmarks/differences/ is written, holding

    side_by_side.png    the whole of both images, expected left, predicted right
    diff.png            black where the two agree, red where they do not
    crop_1_XxY.png      the worst region, expected, predicted and diff, enlarged
    crop_2_XxY.png      the second worst region, and so on

The crop file names carry the top left corner of the region in the image, so a
region can be found again in the full size images.

A whole orienteering map is easily a hundred megapixels, at which size it is
the intermediate results, not the images, which decide how much memory this
needs: turning one into an array of pixels costs three copies of it. Nothing
below therefore holds more than a strip of an image at a time.

Usage: benchmarks/differences.py [options]
"""

import argparse
import shutil
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

# Pillow refuses to open very large images, on the assumption that they are an
# attack rather than a map. These images are our own, and they are that large.
Image.MAX_IMAGE_PIXELS = None


# The colour of the labels and of the background they sit on. The background
# also fills the canvas where one image is smaller than the other, where it is
# dark enough to be told apart from anything the maps are drawn with.
label_background = (32, 32, 32)
label_foreground = (255, 255, 255)

# The colour a differing pixel gets in diff.png and in the crops.
difference_colour = (255, 0, 0)

# How many pixels of an image to compare at a time.
strip_pixels = 4 * 1024 * 1024


def load_font(size):
    """The best available font at the given size, never failing."""
    candidates = (
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "DejaVuSansMono.ttf",
        "DejaVuSans.ttf",
    )
    for candidate in candidates:
        try:
            return ImageFont.truetype(candidate, size)
        except OSError:
            continue
    return ImageFont.load_default()


def compose(panels, labels, *, gap=8):
    """Puts labelled images next to each other on a single canvas.

    The panels keep their size and are aligned at the top, so that images of
    different size stay comparable.
    """
    font_size = max(12, min(40, max(panel.width for panel in panels) // 40))
    font = load_font(font_size)
    bar = font_size + 8

    width = sum(panel.width for panel in panels) + gap * (len(panels) - 1)
    height = max(panel.height for panel in panels) + bar
    canvas = Image.new("RGB", (width, height), label_background)
    draw = ImageDraw.Draw(canvas)

    x = 0
    for panel, label in zip(panels, labels):
        draw.text((x + 4, 4), label, font=font, fill=label_foreground)
        canvas.paste(panel, (x, bar))
        x += panel.width + gap
    return canvas


def open_image(path):
    """Opens an image, in a mode whose first three channels are red, green, blue."""
    image = Image.open(path)
    if image.mode not in ("RGB", "RGBA"):
        image = image.convert("RGB")
    return image


def region(image, box):
    """A part of an image as a height by width by 3 array of pixels.

    Only the part asked for is turned into pixels; the alpha of a rendering,
    where there is one, is left out by taking a view of the first three
    channels rather than by converting the image.
    """
    part = image.crop(box)
    pixels = np.frombuffer(part.tobytes(), dtype=np.uint8)
    return pixels.reshape(part.height, part.width, len(part.getbands()))[:, :, :3]


def difference_mask(expected, predicted, tolerance):
    """Which pixels differ, and by how much at the worst pixel.

    The difference of a pixel is summed over red, green and blue, as the
    image_compare test helper does, so that the same tolerance means the same
    thing in both. Where the two images do not overlap, every pixel counts as
    differing.
    """
    width = max(expected.width, predicted.width)
    height = max(expected.height, predicted.height)
    shared_width = min(expected.width, predicted.width)
    shared_height = min(expected.height, predicted.height)

    mask = np.ones((height, width), dtype=bool)
    largest = 0
    rows = max(1, strip_pixels // max(1, shared_width))
    for y in range(0, shared_height, rows):
        y_end = min(y + rows, shared_height)
        box = (0, y, shared_width, y_end)
        a = region(expected, box).astype(np.int16)
        b = region(predicted, box).astype(np.int16)
        # Three channels, 255 apart at most: the sum stays well inside int16.
        distance = np.abs(a - b).sum(axis=2, dtype=np.int16)
        largest = max(largest, int(distance.max(initial=0)))
        mask[y:y_end, :shared_width] = distance > tolerance
    return mask, largest


def worst_regions(mask, count, size, block=16):
    """The most differing regions first, as (x, y, differing pixels) tuples.

    The image is scored in small blocks, and the highest scoring block which is
    not yet covered by a region becomes the centre of the next one. Regions
    therefore do not overlap, and a single dense cluster of differences does not
    take all of them.

    Without a count every differing pixel ends up inside a region, which is the
    useful default: a difference which is not cropped out is a difference which
    has not been looked at.
    """
    height, width = mask.shape
    size = min(size, height, width)

    blocks_y = -(-height // block)
    blocks_x = -(-width // block)
    scores = np.zeros((blocks_y, blocks_x), dtype=np.int64)
    strip = np.zeros((block, blocks_x * block), dtype=np.uint8)
    for row in range(blocks_y):
        rows = mask[row * block:(row + 1) * block]
        strip[:] = 0
        strip[:rows.shape[0], :width] = rows
        scores[row] = strip.reshape(block, blocks_x, block).sum(axis=(0, 2), dtype=np.int64)

    regions = []
    while not count or len(regions) < count:
        index = int(scores.argmax())
        if scores.flat[index] <= 0:
            break
        block_y, block_x = divmod(index, blocks_x)

        x = min(max(block_x * block + block // 2 - size // 2, 0), width - size)
        y = min(max(block_y * block + block // 2 - size // 2, 0), height - size)
        regions.append((x, y, int(mask[y:y + size, x:x + size].sum())))

        # Do not pick anything inside the region just taken again.
        scores[y // block:-(-(y + size) // block), x // block:-(-(x + size) // block)] = 0
    return regions


def crop(image, x, y, size):
    """The region at the given position, padded where the image ends before it."""
    canvas = Image.new("RGB", (size, size), label_background)
    part = image.crop((x, y, min(x + size, image.width), min(y + size, image.height)))
    canvas.paste(part.convert("RGB") if part.mode != "RGB" else part, (0, 0))
    return canvas


def crop_of_mask(mask, x, y, size):
    """The same region of the difference mask, black and red."""
    part = mask[y:y + size, x:x + size]
    canvas = np.zeros((size, size, 3), dtype=np.uint8)
    canvas[:part.shape[0], :part.shape[1]][part] = difference_colour
    return Image.fromarray(canvas)


def write_crops(expected, predicted, mask, folder, options):
    """Writes the differing regions, enlarged, as expected, predicted and diff.

    The regions come out worst first, and the file names are numbered in that
    order and padded to the same width, so that they stay in it whatever lists
    them.
    """
    size = min(options.crop_size, *mask.shape)
    regions = worst_regions(mask, options.crops, options.crop_size)
    digits = len(str(len(regions)))

    for number, (x, y, differing) in enumerate(regions, start=1):
        panels = [crop(expected, x, y, size),
                  crop(predicted, x, y, size),
                  crop_of_mask(mask, x, y, size)]

        # Nearest neighbour: these differences are a pixel wide, and smoothing
        # them away is exactly what must not happen here.
        zoom = max(1, options.zoom // size)
        if zoom > 1:
            panels = [panel.resize((panel.width * zoom, panel.height * zoom), Image.NEAREST)
                      for panel in panels]

        compose(panels, ["expected", "predicted", f"diff  {differing} pixels"]).save(
            folder / f"crop_{number:0{digits}d}_{x}x{y}.png")


def write_diff(mask, folder):
    """Writes the mask as a black and red image.

    A palette image, because the full size difference of a large map is a
    hundred megapixels of no more than two colours.
    """
    diff = Image.fromarray(mask.view(np.uint8), "P")
    diff.putpalette([0, 0, 0, *difference_colour])
    diff.save(folder / "diff.png")


def whole(path, height, width, limit):
    """The whole image at the path, padded to the given size and scaled to fit.

    The padding shows up only when the two renderings disagree about the size
    of the map. The scaling keeps the overview openable: it is there to show
    where in the map to look, while the crops show the differences themselves.
    """
    image = open_image(path)
    scale = limit / width if limit and width > limit else 1.0
    if scale < 1.0:
        # Resampling an image with an alpha channel makes Pillow premultiply
        # it first, which is another copy of a whole map; dropping the alpha
        # beforehand is the cheaper half of that.
        image = image.convert("RGB").resize(
            (max(1, round(image.width * scale)), max(1, round(image.height * scale))),
            Image.LANCZOS)
    elif image.mode != "RGB":
        image = image.convert("RGB")

    target = (max(1, round(width * scale)), max(1, round(height * scale)))
    if image.size != target:
        canvas = Image.new("RGB", target, label_background)
        canvas.paste(image, (0, 0))
        image = canvas
    return image


def write_side_by_side(paths, sizes, height, width, folder, options):
    """Writes both images whole, expected on the left, predicted on the right.

    The images are read again here, one at a time, rather than kept from the
    comparison: a panel of a large map is hundreds of megabytes, and holding
    both maps and both panels at once is what would decide the memory needed.
    """
    panels = []
    for path, size, label in zip(paths, sizes, ("expected", "predicted")):
        panel = whole(path, height, width, options.overview)
        scaled = "" if panel.width == width else f", shown at {panel.width}x{panel.height}"
        panels.append((panel, f"{label}  {size[0]}x{size[1]}{scaled}"))
    compose([panel for panel, _ in panels], [label for _, label in panels]).save(
        folder / "side_by_side.png")


def main():
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(
        description="Compare the benchmark renderings against the reference images.")
    parser.add_argument("--expected", type=Path, default=here / "expected",
                        help="the reference images (default: benchmarks/expected)")
    parser.add_argument("--predictions", type=Path, default=here / "predictions",
                        help="the rendered images (default: benchmarks/predictions)")
    parser.add_argument("--output", type=Path, default=here / "differences",
                        help="where to write the report (default: benchmarks/differences)")
    parser.add_argument("--tolerance", type=int, default=0, metavar="N",
                        help="per pixel difference, summed over red, green and blue, "
                             "which does not count as a difference (default: 0)")
    parser.add_argument("--crops", type=int, default=0, metavar="N",
                        help="how many regions to crop out, 0 for as many as it "
                             "takes to cover every difference (default: 0)")
    parser.add_argument("--crop-size", type=int, default=128, metavar="N",
                        help="the side of a cropped region, in pixels (default: 128)")
    parser.add_argument("--zoom", type=int, default=512, metavar="N",
                        help="the width a cropped region is enlarged to (default: 512)")
    parser.add_argument("--overview", type=int, default=2000, metavar="N",
                        help="the width the whole images are scaled down to in "
                             "side_by_side.png, 0 for their full size (default: 2000)")
    parser.add_argument("--filter", default="", metavar="TEXT",
                        help="only compare images whose name contains TEXT")
    options = parser.parse_args()

    for folder in (options.expected, options.predictions):
        if not folder.is_dir():
            sys.exit(f"No such directory: {folder}")

    references = sorted(path for path in options.expected.glob("*.png")
                        if options.filter in path.name)
    if not references:
        sys.exit(f"No reference images in {options.expected}")

    options.output.mkdir(parents=True, exist_ok=True)

    identical, missing, differing = 0, [], 0
    for reference in references:
        name = reference.stem
        rendering = options.predictions / reference.name
        folder = options.output / name
        if not rendering.exists():
            missing.append(name)
            continue

        expected = open_image(reference)
        predicted = open_image(rendering)
        mask, largest = difference_mask(expected, predicted, options.tolerance)
        if not mask.any():
            # A pair which used to differ but no longer does must not leave a
            # stale report behind.
            if folder.exists():
                shutil.rmtree(folder)
            identical += 1
            continue

        if folder.exists():
            shutil.rmtree(folder)
        folder.mkdir(parents=True)
        write_crops(expected, predicted, mask, folder, options)
        write_diff(mask, folder)

        # Nothing below needs the maps in memory, and the overview is about to
        # want the room they take.
        sizes = [(expected.width, expected.height), (predicted.width, predicted.height)]
        expected.close()
        predicted.close()
        write_side_by_side((reference, rendering), sizes,
                           mask.shape[0], mask.shape[1], folder, options)

        pixels = int(mask.sum())
        differing += 1
        print(f"{pixels / mask.size:8.4%} {pixels:8d} pixels, largest {largest:3d}  {name}",
              flush=True)

    print()
    print(f"{len(references)} images: {identical} identical, {differing} differing", end="")
    print(f", {len(missing)} not rendered" if missing else "")
    for name in missing:
        print(f"  no rendering for {name}")
    if differing:
        print(f"Wrote {options.output}")


if __name__ == "__main__":
    main()
