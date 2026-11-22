"""Makes `maps/ISSprOM_4k.omap`: Mapper's ISSprOM 2019 (first edition) symbol
set turned into ISSprOM 2019-2 Revision 6 (January 2024), as the IOF document
says (its symbols, its precise definitions and its errata), with the colours
of the IOF Map Specifications, Printing and Colour Definitions, Revision 4
(September 2024).

    python3 maps/tools/issprom_2019_2.py <ISSprOM 2019_4000.omap> maps/ISSprOM_4k.omap

The input is Mapper 0.9.6's "symbol sets/4000/ISSprOM 2019_4000.omap", which
is also this repository's maps/ISSprOM_4k.omap before it was made from it:

    git show cf9549a:maps/ISSprOM_4k.omap > "ISSprOM 2019_4000.omap"

Every change is a text edit of the file, so everything else Mapper keeps is
kept. Dimensions are in micrometres, as Mapper stores them.
"""
import re
import sys

from omapedit import Omap, strip_icon

src, dst = sys.argv[1], sys.argv[2]
o = Omap(open(src, encoding='utf-8').read())

def desc(text):
    return '<description>' + text.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;') + '</description>'

def set_desc(code, text):
    o.sub(code, r'<description>.*?</description>', desc(text).replace('\\', r'\\'), 1) \
        if '<description>' in o.get(code) else \
        o.sub(code, r'(<symbol [^>]*>)', lambda m: m.group(1) + desc(text), 1)

def rename(code, name):
    o.sub(code, r'^(<symbol [^>]*?) name="[^"]*"', lambda m: f'{m.group(1)} name="{name}"', 1)

def recode(code, new_code, name=None):
    o.sub(code, r'^(<symbol [^>]*?) code="[^"]*"', lambda m: f'{m.group(1)} code="{new_code}"', 1)
    if name:
        rename(new_code, name)

def no_icon(code):
    o.put(code, strip_icon(o.get(code)))

def line_attrs(color, width, **kw):
    a = dict(color=color, line_width=width, minimum_length=0, join_style=1, cap_style=0,
             start_offset=0, end_offset=0, segment_length=4000, end_length=0,
             show_at_least_one_symbol='true', minimum_mid_symbol_count=0,
             minimum_mid_symbol_count_when_closed=0, dash_length=4000, break_length=1000,
             dashes_in_group=1, in_group_break_length=500, mid_symbols_per_spot=1,
             mid_symbol_distance=0)
    a.update(kw)
    return ' '.join(f'{k}="{v}"' for k, v in a.items())

def line_symbol(color, width, inner='', **kw):
    if inner:
        return f'<line_symbol {line_attrs(color, width, **kw)}>{inner}</line_symbol>'
    return f'<line_symbol {line_attrs(color, width, **kw)}/>'

def point_element(symbol_xml, coords):
    pts = ';'.join(coords) + ';'
    return (f'<element>{symbol_xml}<object type="1"><coords count="{len(coords)}">{pts}</coords>'
            f'<pattern rotation="0"><coord x="0" y="0"/></pattern></object></element>')

def area_inner(color):
    return f'<symbol type="4" code=""><area_symbol inner_color="{color}" min_area="0" patterns="0"/></symbol>'

def point_xml(elements, rotatable='true', inner_radius=1000, inner_color=-1, outer_width=0, outer_color=-1):
    return (f'<point_symbol rotatable="{rotatable}" inner_radius="{inner_radius}" inner_color="{inner_color}" '
            f'outer_width="{outer_width}" outer_color="{outer_color}" elements="{len(elements)}">'
            + ''.join(elements) + '</point_symbol>')

def lines_pattern(color, width, spacing, angle, rotatable=False):
    rot = ' rotatable="true"' if rotatable else ''
    return (f'<pattern type="1" angle="{angle}"{rot} line_spacing="{spacing}" line_offset="0" '
            f'offset_along_line="0" color="{color}" line_width="{width}"/>')

def area_symbol(color, min_area, patterns=()):
    if not patterns:
        return f'<area_symbol inner_color="{color}" min_area="{min_area}" patterns="0"/>'
    return (f'<area_symbol inner_color="{color}" min_area="{min_area}" patterns="{len(patterns)}">'
            + ''.join(patterns) + '</area_symbol>')

_ids = []

def symbol(type_, code, name, body, description, extra=''):
    """A new symbol, with an id of its own even before it is in the file."""
    _ids.append(max([o.next_id()] + [i + 1 for i in _ids]))
    return (f'<symbol type="{type_}" id="{_ids[-1]}" code="{code}" name="{name}"{extra}>'
            f'{desc(description)}{body}</symbol>')

# ---------------------------------------------------------------- colours ---

# The colours of ISSprOM 2019-2, in their order (the first on top), with their
# CMYK values: IOF Map Specifications, Printing and Colour Definitions,
# Revision 4 (September 2024), page 6. Each is a spot colour ink, or a screen
# of one (as Mapper keeps them), with the CMYK the table gives.
# (name, c, m, y, k, spot composition: an ink name or [(factor, colour)], knockout)
IOF = [
    ('Upper purple for course overprint', 35, 85, 0, 0, 'PURPLE', True),
    ('White for course overprint', 0, 0, 0, 0, [(0, 'Upper purple for course overprint')], True),
    ('Purple 50% area symbol', 18, 43, 0, 0, [(0.5, 'Upper purple for course overprint')], True),
    ('White for railroad', 0, 0, 0, 0, [(0, 'Black 100%')], True),
    ('Black 100%', 0, 0, 0, 100, 'BLACK', False),
    ('Green 100% point symbols', 80, 0, 100, 0, 'GREEN', False),
    ('Lower purple for course overprint', 35, 85, 0, 0, [(1, 'Upper purple for course overprint')], False),
    ('Dark green line symbols', 100, 0, 80, 30, 'DARK GREEN', False),
    ('Blue 100% point and line symbols', 100, 0, 0, 0, 'BLUE', False),
    ('Brown 100%', 25, 75, 100, 0, 'BROWN', False),
    ('White stripes for area passable at two levels', 0, 0, 0, 0, [(0, 'Black 100%')], True),
    ('Brown 50% for road infill', 10, 35, 50, 0, [(0.5, 'Brown 100%')], True),
    ('Brown 30% for road infill', 6, 23, 33, 0, [(0.3, 'Brown 100%')], True),
    ('Black 100% for road outline', 0, 0, 0, 100, [(1, 'Black 100%')], False),
    ('Black 50% for large buildings', 0, 0, 0, 50, [(0.5, 'Black 100%')], True),
    ('Black 20% for canopy', 0, 0, 0, 20, [(0.2, 'Black 100%')], True),
    ('Blue 100% area symbols', 100, 0, 0, 0, [(1, 'Blue 100% point and line symbols')], False),
    ('Blue 70% area symbols', 70, 0, 0, 0, [(0.7, 'Blue 100% point and line symbols')], True),
    ('Blue 50% area symbols', 50, 0, 0, 0, [(0.5, 'Blue 100% point and line symbols')], True),
    ('Blue 30% area symbols', 30, 0, 0, 0, [(0.3, 'Blue 100% point and line symbols')], True),
    ('White over green and brown', 0, 0, 0, 0, [(0, 'Green 100% point symbols')], True),
    ('Brown 50% for paved area', 10, 35, 50, 0, [(0.5, 'Brown 100%')], True),
    ('Brown 30% for paved area', 6, 23, 33, 0, [(0.3, 'Brown 100%')], True),
    ('Olive green', 38, 27, 100, 0, [(0.5, 'Green 100% point symbols'), (1, 'Yellow 100% area symbols')], True),
    ('Dark green area symbols', 100, 0, 80, 30, [(1, 'Dark green line symbols')], False),
    ('Green 100% area symbols', 80, 0, 100, 0, [(1, 'Green 100% point symbols')], False),
    ('Green 60% area symbols', 48, 0, 60, 0, [(0.6, 'Green 100% point symbols')], True),
    ('Green 30% area symbols', 24, 0, 30, 0, [(0.3, 'Green 100% point symbols')], True),
    ('Black 35% area symbols', 0, 0, 0, 35, [(0.35, 'Black 100%')], True),
    ('White over Yellow', 0, 0, 0, 0, [(0, 'Yellow 100% area symbols')], True),
    ('Black for cultivated land and sandy ground', 0, 0, 0, 100, [(1, 'Black 100%')], False),
    ('Yellow 100% area symbols', 0, 27, 79, 0, 'YELLOW', False),
    ('Yellow 50% area symbols', 0, 14, 40, 0, [(0.5, 'Yellow 100% area symbols')], True),
]
# Mapper's set adds two symbols of its own, which are not ISSprOM's: 799
# Simple Orienteering Course and 999 OpenOrienteering Logo. They are left out,
# and with the logo its two colours, which are not IOF colours.
o.remove('799')
o.remove('999')
EXTRA = []
# Each colour of Mapper's ISSprOM 2019 set, by its priority there: the colour
# that takes its place.
OLD = {
    0: 'Upper purple for course overprint', 1: 'White for course overprint', 2: 'Black 100%',
    3: 'Green 100% point symbols', 4: 'White for railroad', 5: 'Lower purple for course overprint',
    6: 'Purple 50% area symbol', 7: 'Blue 100% point and line symbols', 8: 'Brown 100%',
    9: 'Black 50% for large buildings',  # Black 60% (buildings): 50% since 2019-2
    10: 'Black 50% for large buildings',  # Black 50% (tramways, now black 100%)
    11: 'Black 20% for canopy', 12: 'White over green and brown',
    13: 'Brown 50% for road infill', 14: 'Brown 30% for road infill',  # paved areas below
    15: 'Black 100% for road outline', 16: 'Blue 100% area symbols', 17: 'Blue 70% area symbols',
    18: 'Blue 30% area symbols', 19: 'OpenOrienteering Blue', 20: 'OpenOrienteering Orange',
    21: 'White over green and brown', 22: 'Olive green', 23: 'Black 35% area symbols',
    24: 'Green 100% area symbols', 25: 'Green 60% area symbols', 26: 'Green 30% area symbols',
    27: 'Green 100% area symbols',  # Green 100% for undergrowth (407, 409)
    28: 'White over Yellow', 29: 'Black for cultivated land and sandy ground',
    30: 'Yellow 100% area symbols', 31: 'Yellow 50% area symbols',
}
names = [c[0] for c in IOF] + EXTRA
index = {n: i for i, n in enumerate(names)}
old_colors = {int(p): x for p, x in re.findall(r'<color priority="(\d+)" (.*?</color>)', o.t, re.S)}
assert len(old_colors) == 32 and set(OLD) == set(old_colors)

def color_xml(i, name, c, m, y, k, spot, knockout):
    ko = ' knockout="true"' if knockout else ''
    if isinstance(spot, str):
        spots = f'<spotcolors{ko}><namedcolor>{spot}</namedcolor></spotcolors>'
    else:
        spots = f'<spotcolors{ko}>' + ''.join(
            f'<component factor="{f:g}" spotcolor="{index[n]}"/>' for f, n in spot) + '</spotcolors>'
    c, m, y, k = (v / 100 for v in (c, m, y, k))
    r, g, b = ((1 - v) * (1 - k) for v in (c, m, y))
    return (f'<color priority="{i}" name="{name}" c="{c:g}" m="{m:g}" y="{y:g}" k="{k:g}" opacity="1">'
            f'{spots}<cmyk method="custom"/><rgb method="cmyk" r="{r:.3g}" g="{g:.3g}" b="{b:.3g}"/></color>')

table = [color_xml(i, *c) for i, c in enumerate(IOF)]
for name in EXTRA:
    old = old_colors[next(p for p, n in OLD.items() if n == name)]
    # The logo's colours are screens of blue, brown and yellow: their inks now.
    old = re.sub(r'spotcolor="(\d+)"', lambda mm: f'spotcolor="{index[OLD[int(mm.group(1))]]}"', old)
    table.append(f'<color priority="{index[name]}" ' + old)
a, b = o.t.index('<colors '), o.t.index('</colors>')
o.t = o.t[:a] + f'<colors count="{len(table)}">\n' + '\n'.join(table) + '\n' + o.t[b:]

# Every symbol's colours, to the new table.
s0 = o.t.index('<symbols ')
o.t = o.t[:s0] + re.sub(r'\b(color|inner_color|outer_color)="(\d+)"',
                        lambda mm: f'{mm.group(1)}="{index[OLD[int(mm.group(2))]]}"', o.t[s0:])

def C(name):
    return index[name]

BLACK = C('Black 100%')
BLACK50 = C('Black 50% for large buildings')
GREEN_AREA = C('Green 100% area symbols')
DARK_GREEN = C('Dark green area symbols')
DARK_GREEN_LINE = C('Dark green line symbols')
GREEN60 = C('Green 60% area symbols')
GREEN30 = C('Green 30% area symbols')
WHITE_OVER_GREEN = C('White over green and brown')
WHITE_LOWER = C('White stripes for area passable at two levels')
BROWN = C('Brown 100%')
LOWER_PURPLE = C('Lower purple for course overprint')

# Paved areas take the paved area browns, below the road outline; the line
# symbols keep the road infill browns, above it (they cover their own
# outlines where they meet). So the outline of 501 is no longer half under
# its fill: 0.1 mm, as drawn.
o.sub('501.5', r'inner_color="\d+"', f'inner_color="{C("Brown 30% for paved area")}"')
o.sub('501.3', r'area_symbol inner_color="\d+"', f'area_symbol inner_color="{C("Brown 30% for paved area")}"')
o.sub('501.11', r'inner_color="\d+"', f'inner_color="{C("Brown 50% for paved area")}"')
o.sub('501', r'line_width="200"', 'line_width="100"')

# --------------------------------------------------------------- landforms ---

# 105 -> 105.1 Small earth wall: line 0.18 (0.21 here) -> 0.27, dots 0.6 -> 0.7.
recode('105', '105.1')
o.sub('105.1', r'line_width="210"', 'line_width="270"')
o.sub('105.1', r'inner_radius="300"', 'inner_radius="350"')
no_icon('105.1')

# 105.2 Retaining earth wall seen only from one side (new): line 0.27, half
# dots 0.7 every 1.9, their diameter 0.11 from the line's centre.
def half_dot(radius, offset):
    """A half dot of `radius`, its diameter `offset` from the line's centre
    (precise definitions of 105.2 and 513.2), on the right of the line."""
    r, k, d = radius, round(radius * 0.553), offset
    return [f'{-r} {d} 1', f'{-r} {d + k}', f'{-k} {d + r}', f'0 {d + r} 1', f'{k} {d + r}',
            f'{r} {d + k}', f'{r} {d}', f'{-r} {d}', f'{-r} {d} 18']

half = half_dot(350, 110)
o.insert_after('105.1', symbol(2, '105.2', 'Retaining earth wall seen only from one side', line_symbol(
    BROWN, 270, '<mid_symbol><symbol type="1" code="" name="Mid symbol">'
    + point_xml([point_element(area_inner(BROWN), half)]) + '</symbol></mid_symbol>',
    minimum_length=1400, segment_length=1900, end_length=850, mid_symbol_placement=2),
    'A retaining earth wall is an abrupt change in ground level which can be clearly distinguished '
    'from its surroundings used for minor peat edges and cultivation terraces. If such a feature is '
    'higher than 1 m, it shall be drawn with the symbol Earth bank (104).\n'
    'Minimum height: 0.5 m.\nMinimum length (isolated): 1.4 mm (footprint 5.6 m).'))

# 107: "or trench" left out of the name (errata 01.02.2024).
rename('107', 'Erosion gully')

# 113 Broken ground: dots 0.2-0.3 -> 0.3 (errata 01.02.2024).
for code in ['113']:
    o.sub(code, r'inner_radius="100"', 'inner_radius="150"', count=len(re.findall('inner_radius="100"', o.get(code))))
o.sub('113.1', r'inner_radius="100"', 'inner_radius="150"')

# --------------------------------------------------------- rock, boulders ---

# 201, 202: uncrossable; minimum length 1.1 mm (OM) (errata 15.02.2022).
for code in ['201', '201.1', '201.3', '201.4']:
    o.sub(code, r'^(<symbol [^>]*? name=")Impassable cliff', r'\1Uncrossable cliff')
for code in ['201', '201.3', '202', '202.1', '202.3']:
    o.sub(code, r'minimum_length="900"', 'minimum_length="1100"')
# The minimum size symbols: their top line 0.9 -> 1.1 mm, the tags spread alike.
for code in ['201.1', '202.2']:
    old = o.get(code)
    new = old.replace('-450 0;450 0;', '-550 0;550 0;').replace('270 0;270 ', '330 0;330 ').replace('-270 0;-270 ', '-330 0;-330 ')
    assert new.count('550 0;') >= 1 and new.count('330 0;') >= 2, code
    o.put(code, strip_icon(new))

# 206 Gigantic boulder: minimum area 0.75 mm².
o.sub('206', r'min_area="800"', 'min_area="750"')

# 210 Stony ground: dots 0.2-0.25 -> 0.25 (errata 01.02.2024).
o.sub('210', r'inner_radius="100"', 'inner_radius="125"', count=len(re.findall('inner_radius="100"', o.get('210'))))
o.sub('210.1', r'inner_radius="100"', 'inner_radius="125"')

# ------------------------------------------------------------ water, marsh ---

# 301: minimum area (inside) 0.5 mm².
for code in ['301.1', '301.3']:
    o.sub(code, r'min_area="1000"', 'min_area="500"')

# 302: "Shallow body of water" (errata 01.02.2024).
rename('302', 'Shallow body of water')
rename('302.1', 'Shallow body of water, fill')
rename('302.6', 'Shallow body of water, border line')

# ---------------------------------------------------------------- vegetation ---

# 404: dots 0.8 -> 0.7 (errata 01.02.2024).
for code in ['404', '404.1']:
    o.sub(code, r'inner_radius="400"', 'inner_radius="350"')
    no_icon(code)

# 410 Impassable vegetation -> 411 Uncrossable vegetation, dark green
# (errata 02.01.2022). The objects of the reference map keep their symbol.
recode('410', '411', 'Uncrossable vegetation')
o.sub('411', r'inner_color="\d+"', f'inner_color="{DARK_GREEN}"')
set_desc('411', 'Uncrossable vegetation is an area of vegetation (for example a hedge) that shall not be '
         'crossed or passed through since there may be a danger that private property or the vegetation '
         'itself is damaged.\nMinimum area: 0.3 mm² (footprint 5 m²).\nMinimum width: 0.4 mm.')
no_icon('411')
recode('410.1', '411.1', 'Uncrossable vegetation, minimum width (hedge)')
o.sub('411.1', r'color="\d+" line_width="400"', f'color="{DARK_GREEN_LINE}" line_width="400"')
set_desc('411.1', 'Uncrossable vegetation (411) narrower than its minimum width of 0.4 mm, as a hedge.')
no_icon('411.1')

# 410 Vegetation: fight (new meaning): green 100%, minimum width 0.25 mm; for
# better running in one direction, stripes of white / green 30% / green 60%,
# 0.4 wide every 1.5 mm, as 406 and 408 have.
fight = ('An area of dense vegetation (trees or undergrowth) which is barely passable. Running reduced to '
         'less than about 20% of normal speed.\n')
stripes = ('When runnability is better in one direction a regular pattern of white / green 30% / green 60% '
           'stripes is left in the screen to show the direction of better running. At least two stripes '
           'shall be clearly visible.\n')
fight_end = 'Minimum area: 0.3 mm² (footprint 5 m²).\nMinimum width: 0.25 mm.'
new = [
    symbol(4, '410', 'Vegetation: fight', area_symbol(GREEN_AREA, 300), fight + stripes + fight_end),
    symbol(4, '410.2', 'Vegetation: fight, normal running in one direction',
           area_symbol(GREEN_AREA, 300, [lines_pattern(WHITE_OVER_GREEN, 400, 1500, 1.5708, True)]),
           fight + stripes + 'White stripes: normal running in one direction.'),
    symbol(4, '410.3', 'Vegetation: fight, slow running in one direction',
           area_symbol(GREEN30, 300, [lines_pattern(GREEN_AREA, 1100, 1500, 1.5708, True)]),
           fight + stripes + 'Green 30% stripes: slow running in one direction.'),
    symbol(4, '410.4', 'Vegetation: fight, walk in one direction',
           area_symbol(GREEN60, 300, [lines_pattern(GREEN_AREA, 1100, 1500, 1.5708, True)]),
           fight + stripes + 'Green 60% stripes: walk in one direction.'),
]
o.insert_after('409', new[0])
o.insert_after('410', symbol(2, '410.1', 'Vegetation: fight, minimum width', line_symbol(GREEN_AREA, 250),
                             'Vegetation: fight (410) narrower than its minimum width of 0.25 mm.'))
prev = '410.1'
for code, xml in zip(['410.2', '410.3', '410.4'], new[1:]):
    o.insert_after(prev, xml); prev = code

# 415 Distinct cultivation boundary: 0.1 -> 0.15 (errata 01.02.2024).
o.sub('415', r'line_width="100"', 'line_width="150"')

# 417 Prominent large tree: ring 1.0 -> 1.2 (OM), white mask 1.2 -> 1.4
# (errata 02.01.2022).
o.sub('417', r'inner_radius="250"', 'inner_radius="350"')
o.sub('417', r'inner_radius="600"', 'inner_radius="700"')
no_icon('417')

# ---------------------------------------------------------------- man-made ---


# 501.2 (old): paved area in multilevel structures -> 512.3 Area runnable at
# lower level (errata 02.01.2022, 01.02.2024): white stripes 0.3 every 0.6 mm
# at about 45° to the axis of the area, over the symbols of the upper level.
lower = o.get('501.2')
o.remove('501.2')
o.remove('501.12')
lower = strip_icon(lower)
lower = re.sub(r'code="501.2" name="[^"]*"', 'code="512.3" name="Area runnable at lower level"', lower)
lower = re.sub(r'(<pattern type="1"[^>]*?) color="\d+"', rf'\1 color="{WHITE_LOWER}"', lower)
lower = re.sub(r'<description>.*?</description>', desc(
    'Upper part of a two-level structure which can be passed at two levels (except the symbols '
    'Uncrossable body of water (301), Area that shall not be entered (520) and Building (521)). The '
    'angle of the pattern is approximately 45° to the direction of the axis of the described area.\n'
    'Drawn over the symbol of the upper level, in white.').replace('\\', r'\\'), lower, flags=re.S)
assert 'line_width="300"' in lower and 'line_spacing="600"' in lower, lower

# 501.2 Step or edge of paved area at lower level (new): dashed 0.1, dashes
# 0.7, gaps 0.2.
o.insert_after('501.1', symbol(2, '501.2', 'Step or edge of paved area at lower level',
    line_symbol(BLACK, 100, dashed='true', dash_length=700, break_length=200),
    'This symbol shall only be used in large areas with two levels, when it is necessary for '
    'orientation. A cartographic gap of 0.15 mm shall apply at both ends of the line.'))
# Mapper writes dashed="true" right after the cap style.
o.sub('501.2', r' dashed="true"', '')
o.sub('501.2', r'cap_style="0"', 'cap_style="0" dashed="true"')

# 505 Unpaved footpath or track: border 0.1 -> 0.12 (errata 02.01.2022).
for code in ['505.1', '505.2']:
    o.sub(code, r'(<border color="\d+" width=)"100" shift="50"', r'\1"120" shift="60"')

# 509.2 Tramway: 0.1 black 50% -> 0.08 black 100% (errata 01.02.2024).
o.sub('509.2', r'<border color="\d+" width="100"', f'<border color="{BLACK}" width="80"', count=len(re.findall('<border ', o.get('509.2'))))
no_icon('509.2')

# 512.1 Bridge or tunnel entrance (new graphics, errata 02.01.2022): touching
# black triangles of side 0.7, on the right of the line, pointing away from it.
o.remove('512.1')
tri = ['-350 0', '350 0', '0 606', '-350 0']
o.insert_after('512', symbol(2, '512.1', 'Bridge or tunnel entrance', line_symbol(
    -1, 0, '<mid_symbol><symbol type="1" code="" name="Mid symbol">'
    + point_xml([point_element(area_inner(BLACK), tri)]) + '</symbol></mid_symbol>',
    segment_length=700, end_length=350, minimum_mid_symbol_count=1),
    'A bridge is a structure spanning and permitting passage over a river, chasm, road or the like. A '
    'simple bridge, where it is possible to pass underneath, is represented by this symbol, drawn along '
    'the entrance: the triangles are on the right of the line. A cartographic gap of 0.15 mm shall '
    'apply to the black objects pointing towards the peaks of the triangles.\nA simple bridge, which '
    'cannot be passed underneath, shall be represented using symbol Uncrossable wall (515).\n'
    'Minimum: one triangle in complex situation (should be avoided), two triangles in a standalone '
    'situation.'))

# 512.2 Underpass or tunnel: dashes 0.25 x 0.2, gaps 0.2 (errata 02.01.2022).
o.sub('512.2', r'-125 125;125 125;125 -125;-125 -125;', '-125 100;125 100;125 -100;-125 -100;')
o.sub('512.2', r'segment_length="500"', 'segment_length="450"')
no_icon('512.2')

o.insert_after('512.2', lower)

# Lines with dots or tags along them show at least one even when short, or a
# short one reads as another symbol: 513.1 as a plain line, 201 as its top
# line, 518 as 515 (Mapper's 2019 set has no show_at_least_one_symbol here).
for code in ('104', '104.1', '201', '202.1', '513.1', '518'):
    o.sub(code, r'(<line_symbol [^>]*?end_length="\d+")(?! show_at_least_one_symbol)',
          r'\1 show_at_least_one_symbol="true"')
# 311 Small fountain or well has no north arrow in the specification (unlike
# 308, 310 or 313 beside it): a square fountain is drawn turned as it stands.
# Mapper's 2019 set has it square to the paper.
o.sub('311', r'<point_symbol (?!rotatable)', '<point_symbol rotatable="true" ')
# 513.2: "Passable retaining wall", minimum length 2.4 -> 1.4 (errata 01.02.2024).
rename('513.2', 'Passable retaining wall')
o.sub('513.2', r'minimum_length="2400"', 'minimum_length="1400"')

# Uncrossable instead of impassable (errata 02.01.2022).
rename('515', 'Uncrossable wall')
rename('518', 'Uncrossable fence or railing')
rename('529', 'Prominent uncrossable line feature')
rename('526', 'Cairn, memorial, sculpture, small monument or boundary stone')

# 520: minimum area 0.25 mm².
o.sub('520', r'min_area="300"', 'min_area="250"')

# 532 Stairway: black only (border and steps 0.1, minimum 0.4 mm inside, the
# footprint); the border is outside the width in every variant.
for code in ['532.6', '532.7', '532.8', '532.9']:
    o.put(code, o.get(code).replace(re.search(r'<line_symbol color="\d+"', o.get(code)).group(0), '<line_symbol color="-1"', 1))
    o.sub(code, r'(<border color="\d+" width="100") shift="0"', r'\1 shift="50"', count=0 if code == '532.6' else 1)
    no_icon(code)

# 533 Area with obstacles (new): black 50% dots 0.55, 0.75 apart at 45°;
# minimum area 65 m² (4.06 mm²).
dot = f'<symbol type="1" code=""><point_symbol rotatable="false" inner_radius="275" inner_color="{BLACK50}" outer_width="0" outer_color="-1" elements="0"/></symbol>'
o.insert_after('532.9', symbol(4, '533', 'Area with obstacles', area_symbol(-1, 4063, [
    f'<pattern type="2" angle="0.785398" line_spacing="750" line_offset="0" offset_along_line="0" point_distance="750">{dot}</pattern>']),
    'An area with several man-made features that are too small or close to be mapped individually and '
    'that constitute obstacles to the runners. The area cannot be crossed at full speed.\n'
    'Minimum area: 65 m².'))

# ----------------------------------------------------------- course setting ---

# 703 Control point ø 6.0 (CC), 706 Finish ø 5.0 and 7.0 (CC): a ring is drawn
# around its inner radius, so the radius is the centre line's less half the
# 0.35 line (as Mapper's own 799 has them).
o.sub('703', r'inner_radius="3000"', 'inner_radius="2825"')
o.sub('706', r'inner_radius="2500"', 'inner_radius="2325"')
o.sub('706', r'inner_radius="3500"', 'inner_radius="3325"')
no_icon('703'); no_icon('706')

# 709 Out-of-bounds area: hatch 0.25 -> 0.2 every 0.8 -> 1.2 (errata
# 02.01.2022); boundary 0.25 -> 0.4 (errata 01.02.2024).
o.sub('709', r'line_spacing="800"', 'line_spacing="1200"', count=2)
o.sub('709', r'line_width="250"', 'line_width="200"', count=2)
for code in ['709.1', '709.2']:
    o.sub(code, r'line_width="250"', 'line_width="400"')
no_icon('709')

# 710.1 Crossing point: lines 0.35 -> 0.5, 3.0 -> 4.5 mm long (errata
# 01.02.2024), still 1.0 mm apart inside.
o.sub('710.1', r'line_width="350"', 'line_width="500"', count=2)
o.sub('710.1', r'-1500 950 1;-500 675;500 675;1500 950 16;', '-2250 1162 1;-750 750;750 750;2250 1162 16;')
o.sub('710.1', r'-1500 -950 1;-500 -675;500 -675;1500 -950 16;', '-2250 -1162 1;-750 -750;750 -750;2250 -1162 16;')
no_icon('710.1')

# 715 Continuing point after map exchange (new): a 6.0 mm circle (CC), 0.35,
# with an equilateral triangle inscribed, pointing to the next control. Lower
# purple.
r = 3000  # the triangle's corners, on the circle's centre line
tri = [f'0 {-r}', f'{round(r * 0.866)} {r // 2}', f'{-round(r * 0.866)} {r // 2}', f'0 {-r}']
o.insert_after('714.1', symbol(1, '715', 'Continuing point after map exchange', point_xml(
    [point_element(f'<symbol type="2" code="">{line_symbol(LOWER_PURPLE, 350, join_style=0)}</symbol>', tri)],
    inner_radius=2825, inner_color=-1, outer_width=350, outer_color=LOWER_PURPLE),
    'Symbol marking the continuation of the course after the map flip or the map exchange. The triangle '
    'points in the direction of the following control.'))

# 104, 201, 202 tags: 0.75 mm from the centre of the line to the tip (precise
# definitions of symbols: the 0.75 is measured from the line's dotted centre).
def tags_to(code, centre_to_tip):
    old = o.get(code)
    def tag(m):
        (x1, y1), (x2, y2) = [tuple(map(int, p.split())) for p in (m.group(1), m.group(2))]
        if x1 != x2 or max(abs(y1), abs(y2)) not in (500, 600, 650, 750):
            return m.group(0)
        far = lambda y: (centre_to_tip if y > 0 else -centre_to_tip) if abs(y) in (500, 600, 650, 750) else y
        return f'<coords count="2">{x1} {far(y1)};{x2} {far(y2)};</coords>'
    new, n = re.subn(r'<coords count="2">(-?\d+ -?\d+);(-?\d+ -?\d+);</coords>', tag, old)
    n = sum(1 for a, b in zip(re.findall(r'<coords count="2">[^<]*', old), re.findall(r'<coords count="2">[^<]*', new)) if a != b)
    o.put(code, strip_icon(new))
for code in ['104', '104.1', '201', '201.1', '202.1', '202.2']:
    tags_to(code, 750)
# 104: minimum length 0.75 mm.
for code in ['104', '104.1']:
    o.sub(code, r'^(<symbol .*?<line_symbol [^>]*?)minimum_length="\d+"', r'\1minimum_length="750"')

# Minimum lengths: 515 1 mm (errata 16.09.2022), 528 2.2 mm, 529 3 mm.
for code, length in [('515', 1000), ('528', 2200), ('529', 3000)]:
    o.sub(code, r'^(<symbol .*?<line_symbol [^>]*?)minimum_length="0"', rf'\g<1>minimum_length="{length}"')
    no_icon(code)

# Minimum areas: 414 4 mm², 709 9 mm².
for code in ['414', '414.1']:
    o.sub(code, r'min_area="400"', 'min_area="4000"')
o.sub('709', r'min_area="4000"', 'min_area="9000"')

# 419: the white mask 0.25 mm longer than the green cross at its ends.
o.sub('419', r'-600 -600;600 600;', '-689 -689;689 689;')
o.sub('419', r'-600 600;600 -600;', '-689 689;689 -689;')
no_icon('419')

# 115 Prominent landform feature: a triangle 1.35 mm on a side outside its
# 0.25 line (precise definition), so 1.35 - 0.25·√3 between the line's centres
# (it was 0.746, 1.18 outside).
side = 1350 - round(250 * 3 ** 0.5)
h = side * 3 ** 0.5 / 2
half = round(side / 2)
tri = [(-half, round(h / 3)), (half, round(h / 3)), (0, -round(2 * h / 3)), (-half, round(h / 3))]
o.sub('115', r'<coords count="4">[^<]*</coords>', '<coords count="4">' + ';'.join(f'{x} {y}' for x, y in tri) + ';</coords>')
no_icon('115')

# 707 Marked route: dashes 2.0, gaps 0.5.
o.sub('707', r'break_length="750"', 'break_length="500"')
no_icon('707')

# 513.2 half dots: their diameter 0.09 from the line's centre (precise
# definition; here it was on the centre).
o.sub('513.2', r'<coords count="9">[^<]*</coords>', '<coords count="9">' + ';'.join(half_dot(300, 90)) + ';</coords>')
no_icon('513.2')

# 509.1 Railway: black dashes 1.5, white 1.0, between two 0.1 lines 0.35
# apart (CC). The IOF order puts White for railroad above Black 100%, so it
# is drawn as a black line, its two border lines, and white 1.0 dashes over
# it, inside the border lines.
WHITE_RAIL = C('White for railroad')
o.put('509.1', strip_icon(re.sub(r'<combined_symbol parts="2">.*</combined_symbol>',
    lambda m: '<combined_symbol parts="2">'
    f'<part private="true"><symbol type="2" code="509.1.1" name="Railway helper: black line, black borders">'
    + line_symbol(BLACK, 350, '<borders><border color="%d" width="100" shift="0"/></borders>' % BLACK,
                  segment_length=3000, end_length=1500) + '</symbol></part>'
    f'<part private="true"><symbol type="2" code="509.1.2" name="Railway helper: white dashes">'
    + line_symbol(WHITE_RAIL, 250, segment_length=3000, end_length=1500, dashed='true',
                  dash_length=1000, break_length=1500) + '</symbol></part></combined_symbol>',
    o.get('509.1'), flags=re.S)))

# Symbols which are not ISSprOM 2019-2's: Mapper's set carries them over from
# ISOM 2017-2 (dense broken ground, dense boulder field, enlarged stony ground)
# and from ISSOM 2007 (the hidden "from ISSOM" ones), or as variants of its own
# (an enlarged single dot no larger than the single one, north lines with an
# arrow).
for code in ['113.2', '113.6', '201.8', '201.9', '202.8', '202.9', '203.9', '208.5',
             '210.2', '210.3', '601.4', '601.9']:
    o.remove(code)

# The set names itself as the standard it is.
o.t = re.sub(r'(<symbols count="\d+") id="ISSprOM 2019"', r'\1 id="ISSprOM 2019-2"', o.t)
assert 'id="ISSprOM 2019-2"' in o.t
o.renumber()
o.fix_counts()
open(dst, 'w', encoding='utf-8').write(o.t)
print('symbols', len(o._top_symbols()))
