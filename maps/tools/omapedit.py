"""Text-level editing of a Mapper .omap: top-level symbols by code, colours."""
import re

class Omap:
    def __init__(self, text):
        self.t = text

    def _top_symbols(self):
        """(start, end, code) of each top-level symbol, in file order."""
        t = self.t
        a = t.index('<symbols '); b = t.index('</symbols>')
        out = []; i = t.index('>', a) + 1
        while True:
            s = t.find('<symbol ', i, b)
            if s < 0: break
            depth = 0
            for m in re.finditer(r'<symbol\b[^>]*?(/?)>|</symbol>', t[s:b]):
                if m.group(0).startswith('</'): depth -= 1
                elif m.group(1) != '/': depth += 1
                if depth == 0:
                    e = s + m.end(); break
            code = re.match(r'<symbol [^>]*?code="([^"]*)"', t[s:]).group(1)
            out.append((s, e, code)); i = e
        return out

    def block(self, code):
        hits = [x for x in self._top_symbols() if x[2] == code]
        assert len(hits) == 1, (code, len(hits))
        return hits[0]

    def get(self, code):
        s, e, _ = self.block(code); return self.t[s:e]

    def put(self, code, new):
        s, e, _ = self.block(code); self.t = self.t[:s] + new + self.t[e:]

    def sub(self, code, pattern, repl, count=1):
        """Regex-replace inside one symbol; asserts the number of matches."""
        old = self.get(code)
        new, n = re.subn(pattern, repl, old, flags=re.S)
        assert n == count, (code, pattern, n)
        self.put(code, new)

    def remove(self, code):
        s, e, _ = self.block(code)
        self.t = self.t[:s] + self.t[e:].lstrip('\n')

    def insert_after(self, code, new):
        s, e, _ = self.block(code)
        self.t = self.t[:e] + '\n' + new + self.t[e:]

    def next_id(self):
        return max(int(x) for x in re.findall(r'<symbol type="\d+" id="(\d+)"', self.t)) + 1

    def id_of(self, code):
        return re.match(r'<symbol type="\d+" id="(\d+)"', self.get(code)).group(1)

    def renumber(self):
        """Makes each top-level symbol's id its position, as Mapper writes them,
        with the parts of the combined symbols following: Mapper reads a part's
        `symbol` as a position in the list, so a symbol inserted with a new id
        anywhere but at the end would shift the parts onto other symbols (a
        combined symbol made a part of itself crashes Mapper)."""
        tops = self._top_symbols()
        new = {}
        for k, (s, e, _) in enumerate(tops):
            old = re.match(r'<symbol type="\d+" id="(\d+)"', self.t[s:e]).group(1)
            new[old] = str(k)
        a = self.t.index('<symbols '); b = self.t.index('</symbols>')
        body = self.t[a:b]
        body = re.sub(r'(\n<symbol type="\d+" id=")(\d+)"', lambda m: m.group(1) + new[m.group(2)] + '"', body)
        body = re.sub(r'<part symbol="(\d+)"', lambda m: f'<part symbol="{new[m.group(1)]}"', body)
        self.t = self.t[:a] + body + self.t[b:]

    def fix_counts(self):
        n = len(self._top_symbols())
        self.t = re.sub(r'<symbols count="\d+"', f'<symbols count="{n}"', self.t)
        n = len(re.findall(r'<color priority=', self.t))
        self.t = re.sub(r'<colors count="\d+"', f'<colors count="{n}"', self.t)

    def color_index(self, name):
        m = re.search(r'<color priority="(\d+)" name="%s"' % re.escape(name), self.t)
        return int(m.group(1))

    def insert_color(self, before_priority, xml_without_priority):
        """Insert a colour at `before_priority`, shifting every reference."""
        def shift(m):
            v = int(m.group(2))
            return f'{m.group(1)}="{v + 1 if v >= before_priority else v}"'
        self.t = re.sub(r'\b([a-z_]*color|priority)="(\d+)"', shift, self.t)
        anchor = re.search(r'<color priority="%d" ' % (before_priority + 1), self.t).start()
        self.t = self.t[:anchor] + f'<color priority="{before_priority}" ' + xml_without_priority + '\n' + self.t[anchor:]

def strip_icon(sym):
    return re.sub(r'<icon src="[^"]*"/>', '', sym)
