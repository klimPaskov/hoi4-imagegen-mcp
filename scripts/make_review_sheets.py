#!/usr/bin/env python3
"""Review sheets for the person who checks every image of a family before the verdicts are recorded.

  make_review_sheets.py family [family ...] [--out folder]

One PNG per family: the reference images the plans used (vanilla art or the mod's shelf) in the top row, then every
example of the current attempt at native size (enlarged 2x when small) on a dark tile, numbered in brief order.
Reference images are shown only here for comparison and never published.
"""
import json
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from make_gallery_big import MOD, WORK, briefs, load  # noqa: E402

BG = (38, 39, 46)
PAGE = (20, 21, 26)


def tile(path, scale=None):
    im = Image.open(path).convert("RGBA")
    if scale is None:
        scale = 2 if max(im.size) <= 128 else 1
    if scale > 1:
        im = im.resize((im.width * scale, im.height * scale), Image.NEAREST)
    t = Image.new("RGB", im.size, BG)
    t.paste(im, mask=im.getchannel("A"))
    return t


def font(size=12):
    for name in ("arial.ttf", "DejaVuSans.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            pass
    return ImageFont.load_default()


def sheet(family, out):
    items = briefs()[family]
    tiles = []
    refs = []
    for n, item in enumerate(items, 1):
        r = load(family, item["asset"])
        if not r or not r.get("processed"):
            continue
        p = os.path.join(MOD, r["process"]["path"])
        label = f"{n:02d} {item['asset'][:22]}" + (f" a{r.get('attempt', 1)}" if r.get("attempt", 1) > 1 else "")
        tiles.append((label, tile(p)))
        if not refs and not r.get("style"):
            for ref in ((r.get("generate") or {}).get("generation", {}).get("candidates") or [{}])[0].get("references", [])[:6]:
                rp = os.path.join(MOD, ref["path"])
                if os.path.exists(rp):
                    t = tile(rp, 1)
                    t.thumbnail((200, 200))
                    refs.append(("ref " + os.path.basename(ref["path"])[:20], t))
    if not tiles:
        return None
    allt = refs + tiles
    cell_w = max(t.width for _, t in allt) + 12
    cell_h = max(t.height for _, t in allt) + 22
    cols = max(1, min(len(tiles), 1500 // cell_w))
    ref_rows = math.ceil(len(refs) / cols) if refs else 0
    rows = ref_rows + math.ceil(len(tiles) / cols)
    img = Image.new("RGB", (cols * cell_w + 12, rows * cell_h + 12), PAGE)
    d = ImageDraw.Draw(img)
    f = font()
    for i, (label, t) in enumerate(refs):
        x, y = 12 + (i % cols) * cell_w, 12 + (i // cols) * cell_h
        img.paste(t, (x, y + 18))
        d.text((x, y), label, fill=(120, 170, 220), font=f)
    for i, (label, t) in enumerate(tiles):
        x, y = 12 + (i % cols) * cell_w, 12 + (ref_rows + i // cols) * cell_h
        img.paste(t, (x, y + 18))
        d.text((x, y), label, fill=(220, 225, 220), font=f)
    path = os.path.join(out, f"{family}.png")
    img.save(path)
    return path, len(tiles), len(refs)


if __name__ == "__main__":
    args = sys.argv[1:]
    out = os.path.join(WORK, "review")
    if "--out" in args:
        i = args.index("--out")
        out = args[i + 1]
        del args[i : i + 2]
    os.makedirs(out, exist_ok=True)
    for fam in args:
        print(fam, sheet(fam, out))
