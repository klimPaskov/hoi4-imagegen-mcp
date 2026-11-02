#!/usr/bin/env python3
"""Write the per-family gallery pages from the big run (scripts/make_gallery_big.py).

  make_gallery_pages.py [family ...]   images, per-family manifest and README under docs/gallery/<family>/,
                                       the gallery index docs/gallery/README.md, the index entries in
                                       docs/gallery/manifest.json and the links in docs/families.md

Only installed (approved) examples are published. Images are the tool's processed files at native size on a dark tile,
lossless WebP up to 128 px and WebP quality 82 above that. Nothing is retouched.
"""
import hashlib
import json
import os
import re
import sys

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_gallery_docs as old  # noqa: E402  (spec text, naming evidence, ledger-free helpers)
from make_gallery_big import MOD, RES, ROOT, briefs, load  # noqa: E402

OUT = os.path.join(ROOT, "docs", "gallery")
TILE_BG = (38, 39, 46)


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def ledger_index():
    idx = {}
    path = os.path.join(MOD, ".hoi4-imagegen", "ledger.jsonl")
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        idx.setdefault(r.get("asset_id"), []).append({"seq": r["seq"], "kind": r["kind"], "ts": r["ts"], "artifact": r.get("artifact_id"), "hash": r["hash"]})
    return idx


def encode(rec, number, folder):
    src = os.path.join(MOD, rec["process"]["path"])
    img = Image.open(src).convert("RGBA")
    tile = Image.new("RGB", img.size, TILE_BG)
    tile.paste(img, mask=img.getchannel("A"))
    name = f"{number:02d}.webp"
    path = os.path.join(folder, name)
    if max(img.size) <= 128:
        tile.save(path, "WEBP", lossless=True, method=6)
        enc = "lossless WebP"
    else:
        tile.save(path, "WEBP", quality=82, method=6)
        enc = "WebP quality 82"
    return name, path, img.size, enc


def source_info(rec):
    if rec["kind"] == "import":
        cands = []
        for cid in rec.get("stand_in_candidates", []):
            cands.append(cid)
        gen = rec.get("generate", {})
        cand = ((gen.get("generation") or {}).get("candidates") or [{}])[0]
        plan = gen.get("plan") or {}
        return {
            "made_by": "hoi4-imagegen (ChatGPT login) through the gui_texture family at the counter canvas, imported as the counter",
            "plan_id": plan.get("plan_id"),
            "candidate_ids": cands,
            "prompt": (plan.get("prompt") or {}).get("text"),
            "prompt_sha256": (plan.get("prompt") or {}).get("sha256"),
            "cand": cand,
        }
    gen = rec["generate"]
    plan = gen.get("plan") or {}
    cand = gen["generation"]["candidates"][0]
    return {
        "made_by": "hoi4-imagegen",
        "plan_id": plan.get("plan_id"),
        "candidate_ids": [cand["candidate_id"]],
        "prompt": (plan.get("prompt") or {}).get("text"),
        "prompt_sha256": cand.get("prompt_sha256"),
        "cand": cand,
    }


def note_field(note, key):
    m = re.search(key + r": ([^;]+?)(?:;|$)", note or "")
    return m.group(1).strip() if m else None


def build(family, ledger, fams, lk):
    items = briefs()[family]
    folder = os.path.join(OUT, family)
    os.makedirs(folder, exist_ok=True)
    entries = []
    for n, item in enumerate(items, 1):
        rec = load(family, item["asset"])
        if not rec or not rec.get("install") or rec["install"].get("error") or rec.get("validation") != "pass":
            continue
        name, path, size, enc = encode(rec, n, folder)
        si = source_info(rec)
        cand = si["cand"]
        install = rec["install"]
        asset_id = f"{family}.{item['asset']}"
        chain = ledger.get(asset_id, [])
        for cid in rec.get("stand_in_candidates", []):
            pass
        entries.append(
            {
                "number": n,
                "family": family,
                "asset": item["asset"],
                "image": f"docs/gallery/{family}/{name}",
                "sha256": sha(path),
                "encoding": enc,
                "native_size": list(size),
                "style": rec.get("style"),
                "brief": item["brief"],
                "prompt": si["prompt"],
                "prompt_sha256": si["prompt_sha256"],
                "made_by": si["made_by"],
                "plan_id": si["plan_id"],
                "candidate_ids": si["candidate_ids"],
                "provider": cand.get("provider"),
                "model": cand.get("model"),
                "reasoning_effort": "high",
                "image_tool_model": note_field(cand.get("note"), "image tool model \\(chosen by the backend\\)"),
                "image_quality": note_field(cand.get("note"), "image quality"),
                "source_image_size": note_field(cand.get("note"), "image size"),
                "date": rec.get("started_at"),
                "source_pixels_sha256": cand.get("pixels_sha256"),
                "processed_artifact": rec["processed"],
                "processed_pixels_sha256": rec["process"].get("pixels_sha256"),
                "approval_id": (rec.get("review") or {}).get("approval_id"),
                "installed_files": [{"path": f["path"], "sha256": f["sha256"]} for f in install.get("final_files", [])],
                "sprite_names": (install.get("sprite") or {}).get("names") or [],
                "script": install.get("script"),
                "ledger": chain,
            }
        )
    if not entries:
        return None
    meta = {
        "family": family,
        "about": "Real output of hoi4-imagegen on the ChatGPT login (plan quota, no money). Images are the tool's processed files at native size on a dark tile.",
        "game": "Hearts of Iron IV 1.19.4.1",
        "examples": entries,
    }
    with open(os.path.join(folder, "manifest.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(meta, f, indent=1, ensure_ascii=False)
        f.write("\n")
    page(family, entries, fams[family], lk[family], folder)
    return entries


def page(family, entries, fam, lookup, folder):
    w = []
    a = w.append
    size = old.canvas_text(fam)
    frames = fam["frames"]
    if frames > 1:
        size += f" per frame, {frames} frames in a {fam['strip'][0]}x{fam['strip'][1]} strip"
    else:
        size += ", 1 frame"
    a(f"# {family}: {len(entries)} examples\n")
    a(f"[All families](../README.md) · [Reference entry in the gallery document](../../families.md#{family}) · [Reference](../../reference.md)\n")
    a("| | |")
    a("|---|---|")
    a(f"| Group and tool | {fam['group']}, `{fam['tool']}` |")
    a(f"| Canvas and frames | {size}; {fam['container'].upper()} |")
    a(f"| Names the game looks up | {old.KIND[lookup['kind']]}. Example: " + (", ".join(f"`{n}`" for n in entries[0]["sprite_names"]) or "the file name") + f". Script: {entries[0]['script'] or lookup['script']} |")
    rec = fam["recipe"]
    a(f"| Processing | recipe `{rec['id']}` v{rec['version']}: {old.RECIPES.get(rec['id'], '')} |")
    a("| Checks | " + ", ".join(f"`{c}`" for c in fam["checks"]) + " |")
    styled = sum(1 for e in entries if e["style"])
    a(f"| Style | {len(entries) - styled} use the references the plan picks on its own (the kit's shelf and vanilla art), {styled} use a pinned mod style (`linocut`, `gouache` or `ink`, each with two reference images) |")
    a("")
    first = entries[0]
    scale = 2 if max(first["native_size"]) <= 128 else 1
    shown = "enlarged 2x" if scale == 2 else "at native size"
    a(
        f"Every image is a real `hoi4-imagegen` output on the ChatGPT login, planned, generated, processed, validated, reviewed and installed by the tool. They are shown {shown} on a dark tile and are not retouched. "
        "Each example's brief, prompt, plan id, model, date, SHA-256 and ledger record are in [`manifest.json`](manifest.json).\n"
    )
    if fam["automation"] == "import_only":
        a(
            "This family is import-only: the tool does not generate counters or emblems, because their second frame is different art. "
            "Each frame here was generated by the tool's own provider through the `gui_texture` family at the exact canvas and then imported with `hoi4.image_process`.\n"
        )
    wdisp = max(e["native_size"][0] for e in entries) * scale
    cols = max(1, min(6, 940 // (wdisp + 24)))
    a("<table>")
    for i in range(0, len(entries), cols):
        a("<tr>")
        for e in entries[i : i + cols]:
            wd = e["native_size"][0] * scale
            style = f"<br>{e['style']}" if e["style"] else ""
            a(f'<td align="center" valign="top"><img src="{os.path.basename(e["image"])}" width="{wd}" alt="{e["asset"]}"><br><sub>{e["number"]:02d} {e["asset"]}{style}</sub></td>')
        a("</tr>")
    a("</table>\n")
    a("## Briefs\n")
    a("| # | Asset | Brief |")
    a("|---|---|---|")
    for e in entries:
        a(f"| {e['number']:02d} | `{e['asset']}` | {e['brief'].replace('|', '/')} |")
    a("")
    with open(os.path.join(folder, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(w))


def index(all_entries, fams):
    w = []
    a = w.append
    total = sum(len(v) for v in all_entries.values())
    a("# Gallery of real examples\n")
    a(
        f"{total} real example images across {len(all_entries)} of the 40 families, all made by `hoi4-imagegen` on the [ChatGPT login](../chatgpt-login.md) and shown at native size. "
        "Each family has its own page with a grid of examples, its spec summary and the brief behind every image. "
        "The [gallery document](../families.md) has the one headline example per family with the full reference, and the contact sheet below shows them all.\n"
    )
    a('<p align="center"><a href="contact-sheet.webp"><img src="contact-sheet.webp" alt="One example of each of the 40 families" width="100%"></a></p>\n')
    from make_gallery_docs import GROUPS

    for key, title, tool in GROUPS:
        names = [f for f in all_entries if fams[f]["group"] == key]
        if not names:
            continue
        a(f"## {title}\n")
        a(f"| Family | Examples | Canvas |")
        a("|---|---|---|")
        for n in names:
            a(f"| [`{n}`]({n}/README.md) | {len(all_entries[n])} | {old.canvas_text(fams[n])} |")
        a("")
    with open(os.path.join(OUT, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(w))


def link_families_md(all_entries):
    path = os.path.join(ROOT, "docs", "families.md")
    text = open(path, encoding="utf-8").read()
    for fam, entries in all_entries.items():
        line = f"**[All {len(entries)} examples of `{fam}`](gallery/{fam}/README.md)**\n"
        pat = re.compile(rf"(### {re.escape(fam)}\n\n)(\*\*\[All \d+ examples of `{re.escape(fam)}`\]\(gallery/{re.escape(fam)}/README.md\)\*\*\n\n)?")
        text, n = pat.subn(lambda m: m.group(1) + line + "\n", text, count=1)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def main(only):
    fams = {f["family"]: old.cli("families", f["family"])["family"] for f in old.cli("families")["families"]}
    lk = old.lookups()
    ledger = ledger_index()
    all_entries = {}
    for fam in only or list(briefs()):
        e = build(fam, ledger, fams, lk)
        if e:
            all_entries[fam] = e
            print(fam, len(e), "examples")
    # the index lists every family that has a manifest on disk, not only this run's
    for fam in briefs():
        mp = os.path.join(OUT, fam, "manifest.json")
        if fam not in all_entries and os.path.exists(mp):
            all_entries[fam] = json.load(open(mp, encoding="utf-8"))["examples"]
    ordered = {f: all_entries[f] for f in sorted(all_entries, key=lambda f: [x["family"] for x in old.cli("families")["families"]].index(f))}
    index(ordered, fams)
    link_families_md(ordered)
    mpath = os.path.join(OUT, "manifest.json")
    top = json.load(open(mpath, encoding="utf-8"))
    top["family_galleries"] = {f: {"manifest": f"docs/gallery/{f}/manifest.json", "page": f"docs/gallery/{f}/README.md", "examples": len(v)} for f, v in ordered.items()}
    with open(mpath, "w", encoding="utf-8", newline="\n") as f:
        json.dump(top, f, indent=1, ensure_ascii=False)
        f.write("\n")


if __name__ == "__main__":
    main(sys.argv[1:])
