"""Cross-checks fonter-core coverage against an independent fontTools computation.

usage: python verify_coverage.py <inspect.exe> [font files...]

A code point counts as covered when cmap maps it to a glyph that actually draws
something. Separators and control/format/private-use code points (categories
Z* and C*) are skipped: Fonter never queries them.
"""
import os, subprocess, sys, unicodedata
from fontTools.ttLib import TTFont
from fontTools.pens.boundsPen import ControlBoundsPen

INVISIBLE = {0x115F, 0x1160, 0x2800, 0x3164, 0xFFA0, 0x034F}

def queryable(cp):
    return unicodedata.category(chr(cp))[0] not in "ZC" and not (0xFE00 <= cp <= 0xFE0F) and not (0x180B <= cp <= 0x180F)

def expected(path, index):
    font = TTFont(path, fontNumber=index, lazy=True)
    cmap = font.getBestCmap() or {}
    glyphs = font.getGlyphSet()
    notdef = font.getGlyphOrder()[0]
    blank = {}
    def is_blank(name):
        if name not in blank:
            pen = ControlBoundsPen(glyphs)
            try:
                glyphs[name].draw(pen)
                blank[name] = pen.bounds is None
            except Exception:
                blank[name] = False
        return blank[name]
    return {cp for cp, g in cmap.items()
            if queryable(cp) and g != notdef and (not is_blank(g) or cp in INVISIBLE)}

exe, files = sys.argv[1], sys.argv[2:]
out = subprocess.run([exe, *files], capture_output=True, text=True, encoding="utf-8",
                     env=dict(os.environ, FONTER_RANGES="1")).stdout.splitlines()
bad = 0
for head, ranges in zip(out[0::2], out[1::2]):
    path, index, family, style, _, _ = head.split("\t")
    got = set()
    for r in ranges.split():
        a, b = r.split("-")
        got.update(cp for cp in range(int(a, 16), int(b, 16) + 1) if queryable(cp))
    want = expected(path, int(index))
    ok = got == want
    bad += not ok
    extra = "" if ok else f"  only-fonter={sorted(got - want)[:6]} only-fontTools={sorted(want - got)[:6]}"
    print(("OK  " if ok else "DIFF"), f"{family} {style}: {len(got)} code points{extra}")
sys.exit(1 if bad else 0)
