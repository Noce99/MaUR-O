#!/usr/bin/env python3
"""Builds almost_ISSprOM: ISSprOM.omap without the symbols a network cannot
learn yet, and the table of what a network is to see in the rest.

    python3 symbols_sets/make_almost_issprom.py

writes, beside this script,

* ``almost_ISSprOM.omap`` -- ``ISSprOM.omap`` less the see-through areas,
  the course overprint, the lettering and the minimum-size copies of other
  symbols, plus one line no symbol of the set draws alone (the dashed border
  of an unpaved track), renumbered so that every id is its position again,
  which is what Mapper wants;
* ``almost_ISSprOM.labels.json`` -- the classes a network trained on maps of
  that set has a channel for, and, for every symbol of it, which of those
  classes an object of the symbol is labelled with (see
  ``src/label_set.rs``).

The table is built on two rules. A symbol made of parts is labelled as its
parts: a building is its grey fill and its black outline, a road of a given
footprint is a strip of paved area and its two black borders. And two symbols
which look the same are one class, since a network can only be asked what a
picture shows: the bank line of a lake, the border line of a marsh and the
tag lines of a cliff are all a black line 0.18 mm wide. Every such merge is
checked here against the symbols themselves, so a change to ISSprOM.omap
which makes two of them differ stops this script rather than the training.
"""

import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = HERE / "ISSprOM.omap"
TARGET = HERE / "almost_ISSprOM.omap"
TABLE = HERE / "almost_ISSprOM.labels.json"

# What a network cannot learn from these maps yet, left out of the set.
LEFT_OUT = {
    # See-through areas, and the symbols which have one as a part.
    "113", "208", "210", "307", "307.1", "308", "308.1", "310", "310.1",
    "407", "409", "412", "412.1", "512.3", "533", "601.1", "601.6", "709",
    # The course overprint: drawn on a map, but not the map.
    "701", "702", "703", "704", "704.1", "705", "706", "707", "708",
    "709.1", "709.2", "710.1", "710.2", "714", "714.1", "715",
    # Lettering.
    "102.2",
    # Minimum-size symbols which are another symbol drawn small: a short
    # cliff, a short rock face, a small building.
    "201.1", "202.2", "521.1",
}

# The one symbol added: a black dashed line 0.12 mm wide, the border of an
# unpaved track (505.1, 505.2), which no symbol of ISSprOM draws on its own.
# It is the class those borders are labelled as, and a network's answer for
# that class needs a symbol to be written back with.
TRACK_BORDER_CODE = "505.6"
TRACK_BORDER = (
    '<symbol type="2" id="{id}" code="505.6" name="Unpaved footpath or track, border line">'
    "<description>The dashed border of an unpaved footpath or track (505.1, 505.2), "
    "on its own. Not an ISSprOM 2019-2 symbol: almost_ISSprOM labels the borders of a "
    "track with it.</description>"
    '<line_symbol color="13" line_width="120" minimum_length="0" join_style="1" '
    'cap_style="0" dashed="true" start_offset="0" end_offset="0" segment_length="4000" '
    'end_length="0" show_at_least_one_symbol="true" minimum_mid_symbol_count="0" '
    'minimum_mid_symbol_count_when_closed="0" dash_length="2000" break_length="250" '
    'dashes_in_group="1" in_group_break_length="500" mid_symbols_per_spot="1" '
    'mid_symbol_distance="0"/></symbol>'
)

# The classes, as (name, the symbol a network's answer is written back with).
AREAS = [
    ("Open land", "401"),
    ("Open land with scattered trees", "402"),
    ("Open land with scattered bushes", "402.1"),
    ("Rough open land", "403"),
    ("Rough open land with scattered trees", "404"),
    ("Rough open land with scattered bushes", "404.1"),
    ("Forest", "405"),
    ("Vegetation: slow running", "406"),
    ("Vegetation: slow running, normal running in one direction", "406.1"),
    ("Vegetation: walk", "408"),
    ("Vegetation: walk, normal running in one direction", "408.1"),
    ("Vegetation: walk, slow running in one direction", "408.2"),
    ("Vegetation: fight", "410"),
    ("Vegetation: fight, normal running in one direction", "410.2"),
    ("Vegetation: fight, slow running in one direction", "410.3"),
    ("Vegetation: fight, walk in one direction", "410.4"),
    ("Uncrossable vegetation", "411"),
    ("Orchard", "413"),
    ("Orchard, rough open land", "413.1"),
    ("Vineyard or similar", "414"),
    ("Vineyard or similar, rough open land", "414.1"),
    ("Open sandy ground", "213"),
    ("Bare rock", "214"),
    ("Gigantic boulder or rock pillar", "206"),
    ("Uncrossable body of water (full colour)", "301.1"),
    ("Uncrossable body of water (dominant)", "301.3"),
    ("Shallow body of water", "302.1"),
    ("Paved area, light traffic", "501.5"),
    ("Paved area with scattered trees", "501.3"),
    ("Paved area, heavy traffic", "501.11"),
    ("Area that shall not be entered", "520"),
    ("Building", "521.3"),
    ("Canopy", "522.3"),
]

LINES = [
    # Brown.
    ("Contour", "101"),
    ("Index contour", "102"),
    ("Form line", "103"),
    ("Earth bank", "104"),
    ("Earth bank, very high", "104.1"),
    ("Small earth wall", "105.1"),
    ("Retaining earth wall", "105.2"),
    ("Erosion gully", "107"),
    ("Small erosion gully", "108"),
    # Black.
    ("Uncrossable cliff", "201"),
    ("Black line 0.50 mm", "201.3"),
    ("Passable rock face", "202.1"),
    ("Black line 0.30 mm", "202.3"),
    ("Black line 0.18 mm", "301.6"),
    ("Black line 0.14 mm", "415"),
    ("Black line 0.10 mm", "501.1"),
    ("Black dashed line 0.10 mm", "501.2"),
    ("Black dashed line 0.12 mm", TRACK_BORDER_CODE),
    ("Distinct vegetation boundary", "416"),
    ("Small unpaved footpath or track", "506"),
    ("Less distinct small path", "507"),
    ("Narrow ride", "508"),
    ("Railway", "509.1"),
    ("Tramway", "509.2"),
    ("Power line, cableway or skilift", "510"),
    ("Major power line", "511"),
    ("Major power line with pylons", "511.1"),
    ("Bridge or tunnel entrance", "512.1"),
    ("Underpass or tunnel", "512.2"),
    ("Passable wall", "513.1"),
    ("Passable retaining wall", "513.2"),
    ("Uncrossable wall", "515"),
    ("Passable fence or railing", "516"),
    ("Uncrossable fence or railing", "518"),
    ("Prominent line feature", "528"),
    ("Prominent uncrossable line feature", "529"),
    ("Stairway", "532.7"),
    # Blue.
    ("Blue line 0.18 mm", "302.6"),
    ("Small crossable watercourse", "305"),
    ("Minor / seasonal watercourse", "306"),
    ("Narrow marsh", "309"),
]

POINTS = [
    ("Slope line", "101.1"),
    ("Small knoll", "109"),
    ("Small elongated knoll", "110"),
    ("Small depression", "111"),
    ("Pit or hole", "112"),
    ("Prominent landform feature", "115"),
    ("Rocky pit or cave", "203"),
    ("Cave", "203.1"),
    ("Boulder", "204"),
    ("Large boulder", "205"),
    ("Boulder cluster", "207"),
    ("Boulder cluster, large", "207.1"),
    ("Boulder field, single triangle", "208.1"),
    ("Boulder field, single triangle, enlarged", "208.2"),
    ("Waterhole", "303"),
    ("Small fountain or well", "311"),
    ("Spring", "312"),
    ("Prominent water feature", "313"),
    ("Prominent large tree", "417"),
    ("Prominent bush or small tree", "418"),
    ("Prominent vegetation feature", "419"),
    ("Crossing point", "519"),
    ("Pillar", "522.1"),
    ("High tower", "524"),
    ("Small tower", "525"),
    ("Cairn, memorial or boundary stone", "526"),
    ("Fodder rack", "527"),
    ("Prominent man-made feature – ring", "530"),
    ("Prominent man-made feature – x", "531"),
]

# How many turns of a rotatable point symbol look alike, where more than one:
# an angle is only learnable up to what the picture shows of it. An ellipse
# and a pair of parallel bars look the same half a turn round, a square a
# quarter of one.
POINT_SYMMETRY = {"110": 2, "519": 2, "311": 4, "522.1": 4}

# Every symbol whose labels are not simply its own class, as parts. A part is
# one of
#   ("area", class)    an area object's shape, or the strip a line object's
#                      main stroke covers
#   ("outline", class) an area object's boundary, as a line
#   ("line", class)    a line object's path
#   ("borders", class) a line object's borders, each at its own offset
#   ("point", class)   a point object
# and a symbol with no parts is one which is only ever a part of another, and
# not drawn on its own.
B10, B14, B18, B30 = (
    "Black line 0.10 mm", "Black line 0.14 mm", "Black line 0.18 mm", "Black line 0.30 mm",
)
LIGHT, HEAVY = "Paved area, light traffic", "Paved area, heavy traffic"
PARTS = {
    # Combined areas, as their parts.
    "301": [("area", "Uncrossable body of water (full colour)"), ("outline", B18)],
    "301.2": [("area", "Uncrossable body of water (dominant)"), ("outline", B18)],
    "302": [("area", "Shallow body of water"), ("outline", "Blue line 0.18 mm")],
    "501": [("area", LIGHT), ("outline", B10)],
    "501.10": [("area", HEAVY), ("outline", B10)],
    "521": [("area", "Building"), ("outline", B14)],
    "522": [("area", "Canopy"), ("outline", B10)],
    # Parts which are not drawn on their own: a fill without the line the
    # standard always draws round it.
    "301.1": [], "301.3": [], "302.1": [], "521.3": [], "521.4": [],
    "522.3": [], "522.4": [],
    # Roads of a fixed footprint: a strip of paved area and its borders,
    # which is what a paved area of that width with its border line is too.
    **{code: [("area", LIGHT), ("borders", B10)]
       for code in ("501.6", "501.7", "501.8", "501.9")},
    **{code: [("area", HEAVY), ("borders", B10)]
       for code in ("501.16", "501.17", "501.18", "501.19")},
    **{code: [("area", LIGHT), ("borders", "Black dashed line 0.12 mm")]
       for code in ("505.1", "505.2")},
    # Stairways: the steps along the middle and the two black borders; the
    # four footprints are one class.
    **{code: [("line", "Stairway"), ("borders", B10)]
       for code in ("532.6", "532.7", "532.8", "532.9")},
    # Vegetation drawn narrow: a strip of the area of that vegetation.
    "406.2": [("area", "Vegetation: slow running")],
    "408.3": [("area", "Vegetation: walk")],
    "410.1": [("area", "Vegetation: fight")],
    "411.1": [("area", "Uncrossable vegetation")],
    # Lines which look like another.
    "104.2": [("line", "Contour")],
    "202": [("line", B30)],
    "201.4": [("line", B18)],
    "202.4": [("line", B18)],
    "307.2": [("line", B18)],
    "520.1": [("line", B10)],
    "601": [("line", B14)],
    "601.5": [("line", "Blue line 0.18 mm")],
    # The three slope lines, one class: how thick it is drawn says which
    # contour it hangs off, and the contour is there to be seen.
    "102.1": [("point", "Slope line")],
    "103.1": [("point", "Slope line")],
}

M = "{http://openorienteering.org/apps/mapper/xml/v2}"


def split(text):
    """The file as (head, [symbol blocks], tail): a block is one top-level
    `<symbol>` element, which this file starts on a line of its own."""
    start = text.index("<symbols ")
    start = text.index("\n", start) + 1
    end = text.index("</symbols>")
    body = text[start:end]
    starts = [m.start() for m in re.finditer(r"^<symbol ", body, re.M)]
    blocks = [body[a:b] for a, b in zip(starts, starts[1:] + [len(body)])]
    return text[:start], blocks, text[end:]


def element(block):
    return ET.fromstring(block.strip())


def code_of(block):
    return element(block).get("code").strip()


def rgb(colors, priority):
    return colors[int(priority)]


def look(colors, line):
    """What a plain line symbol looks like, as far as telling two apart goes:
    its ink, its width to the hundredth of a mm, and whether it is dashed."""
    return (
        rgb(colors, line.get("color")),
        round(int(line.get("line_width")) / 1000, 2),
        line.get("dashed") == "true",
    )


def main():
    text = SOURCE.read_text(encoding="utf-8")
    head, blocks, tail = split(text)
    root = ET.fromstring(text)
    colors = {}
    for color in root.iter(M + "color"):
        rgb_of = color.find(M + "rgb")
        colors[int(color.get("priority"))] = tuple(
            round(float(rgb_of.get(c)), 3) for c in "rgb"
        )

    by_code = {}
    for block in blocks:
        code = code_of(block)
        if code in by_code:
            sys.exit(f"{SOURCE.name} has two symbols numbered {code}")
        by_code[code] = block
    for code in LEFT_OUT | set(PARTS):
        if code not in by_code:
            sys.exit(f"{SOURCE.name} has no symbol {code}")

    # The merges this table makes, checked against the symbols.
    def plain(code):
        return element(by_code[code]).find("line_symbol")

    same = [
        ("101", "104.2"),
        ("202", "202.3"),
        ("301.6", "307.2"), ("301.6", "201.4"), ("301.6", "202.4"),
        ("501.1", "520.1"), ("501.1", "522.4"),
        ("302.6", "601.5"),
    ]
    for a, b in same:
        if look(colors, plain(a)) != look(colors, plain(b)):
            sys.exit(f"{a} and {b} no longer look alike: {look(colors, plain(a))} "
                     f"against {look(colors, plain(b))}")
    # 0.14 and 0.15 mm, a tenth of a pixel apart at three pixels a meter.
    for code in ("521.4", "601"):
        a, b = look(colors, plain("415")), look(colors, plain(code))
        if a[0] != b[0] or a[2] != b[2] or abs(a[1] - b[1]) > 0.011:
            sys.exit(f"415 and {code} no longer look alike: {a} against {b}")
    # The road and track infills are the paved areas' own ink, the narrow
    # vegetation the vegetation's, and every border a 0.10 mm black line
    # (0.12 dashed for a track) just outside the strip.
    area_ink = {name: rgb(colors, element(by_code[code]).find("area_symbol").get("inner_color"))
                for name, code in AREAS}
    for code, parts in PARTS.items():
        if not parts or parts[0][0] != "area" or element(by_code[code]).get("type") != "2":
            continue
        line = plain(code)
        if rgb(colors, line.get("color")) != area_ink[parts[0][1]]:
            sys.exit(f"{code} is not drawn in the ink of {parts[0][1]}")
        for border in line.iter("border"):
            width = int(border.get("width"))
            want = (120, "true") if "dashed" in parts[1][1] else (100, None)
            if (width, border.get("dashed")) != want or int(border.get("shift")) != width // 2:
                sys.exit(f"{code}'s border is not a {parts[1][1]} just outside the strip")

    # The set: what is left, the track border after the tracks, and every id
    # its position again, with the combined symbols' parts renumbered to match.
    kept = [block for block in blocks if code_of(block) not in LEFT_OUT]
    at = next(i for i, block in enumerate(kept) if code_of(block) == "505.2") + 1
    kept.insert(at, TRACK_BORDER.format(id="NEW") + "\n")
    old_id = {}
    for block in kept:
        found = re.match(r'<symbol [^>]*?id="([^"]*)"', block)
        old_id[found.group(1)] = len(old_id)
    out = []
    for position, block in enumerate(kept):
        block = re.sub(r'^(<symbol [^>]*?)id="[^"]*"', rf'\g<1>id="{position}"', block, count=1)

        def part(found):
            old = found.group(1)
            if old not in old_id:
                sys.exit(f"a kept symbol is made of symbol {old}, which is left out")
            return f'<part symbol="{old_id[old]}"/>'

        block = re.sub(r'<part symbol="([^"]*)"/>', part, block)
        out.append(block)
    head = re.sub(r'<symbols count="\d+"', f'<symbols count="{len(out)}"', head)
    head = head.replace('id="ISSprOM 2019-2"', 'id="almost ISSprOM 2019-2"')
    # The sample objects of the source go: some name symbols left out.
    tail = re.sub(r"<objects count=\"\d+\">.*?</objects>", '<objects count="0"></objects>',
                  tail, flags=re.S)
    TARGET.write_text(head + "".join(out) + tail, encoding="utf-8")

    # The table.
    kept_codes = [code_of(block) for block in out]
    classes = {"area": AREAS, "line": LINES, "point": POINTS}
    names = {kind: {name for name, _ in listed} for kind, listed in classes.items()}
    for kind, listed in classes.items():
        if len(names[kind]) != len(listed):
            sys.exit(f"two {kind} classes share a name")
        for name, code in listed:
            if code not in kept_codes:
                sys.exit(f"the {kind} class {name} is written back as {code}, which is left out")
    layer = {"area": "area", "outline": "line", "line": "line", "borders": "line",
             "point": "point"}
    symbols = {}
    own = {code: (kind, name) for kind, listed in classes.items() for name, code in listed}
    for code in kept_codes:
        if code in PARTS:
            parts = PARTS[code]
        elif code in own:
            kind, name = own[code]
            parts = [(kind, name)]
        else:
            sys.exit(f"symbol {code} is neither a class nor labelled as parts")
        for part, name in parts:
            if name not in names[layer[part]]:
                sys.exit(f"symbol {code} is labelled with {name}, which is no {layer[part]} class")
        symbols[code] = [{part: name} for part, name in parts]
    table = {
        "symbol_set": TARGET.name,
        "areas": [{"name": name, "symbol": code} for name, code in AREAS],
        "lines": [{"name": name, "symbol": code} for name, code in LINES],
        "points": [
            {"name": name, "symbol": code, **({"symmetry": POINT_SYMMETRY[code]}
                                              if code in POINT_SYMMETRY else {})}
            for name, code in POINTS
        ],
        "symbols": symbols,
    }
    TABLE.write_text(json.dumps(table, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    drawn = sum(1 for parts in symbols.values() if parts)
    print(f"{TARGET.name}: {len(out)} symbols, {drawn} of them drawn on their own")
    print(f"{TABLE.name}: {len(AREAS)} areas, {len(LINES)} lines, {len(POINTS)} points")


if __name__ == "__main__":
    main()
