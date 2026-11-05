#!/usr/bin/env python3
"""A review batch: two examples of every family to check against vanilla references, plus earlier handbacks.

  make_rating_batch.py gen                 generate 2 unstyled examples per family (serial, resumable; they are also
                                           the gallery's first examples, so nothing is spent twice)
  make_rating_batch.py review <verdicts.json>
                                           record hoi4.image_review for every image from a person's verdict file:
                                           {"<family>/<asset>": {"verdict": "approved" | "changes_requested",
                                           "fail": ["<rubric key>", ...], "notes": "..."}}
  make_rating_batch.py export <folder> [<handback folder> <id> ...]
                                           write <folder>/: native image, 4x nearest-neighbour copy, one vanilla
                                           reference per family and manifest.json. Each extra <id> is a handback
                                           folder (result.json from make_gallery_big.py extra) to include.

Environment as for make_gallery_big.py (GALLERY_MOD, HOI4_IMAGEGEN_BIN, GALLERY_RES, GALLERY_EXPECT_VERSION).
Failures are kept: an example that fails validation or review is exported with its result, never left out.
"""
import json
import os
import re
import shutil
import sys

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_gallery_big as m  # noqa: E402

PER_FAMILY = 2
NATIONS = [
    "Finnish", "Brazilian", "Turkish", "Polish", "Japanese", "Chinese", "British", "American", "Soviet", "Italian", "French",
    "Ethiopian", "Persian", "Swedish", "Argentine", "Yugoslav", "Spanish", "Australian", "Thai", "Mexican", "German", "Indian",
    "Irish", "Greek", "Romanian", "Hungarian", "Czech", "Dutch", "Belgian", "Norwegian", "Portuguese", "Canadian", "Egyptian",
]


def picks():
    """The first PER_FAMILY unstyled briefs of each family, in the order the briefs file lists the families."""
    out = []
    for fam, items in m.briefs().items():
        chosen = [i for i in items if not i.get("style")][:PER_FAMILY]
        out += [{"family": fam, **i} for i in chosen]
    return out


def jobs_for_counters(job):
    if job["family"] in m.COUNTERS:
        job = dict(job)
        job["brief"] = re.sub(r"\bwhite\b", "black", job["brief"], flags=re.I)
    return job


def cmd_gen(args):
    m.check_build()
    todo = [jobs_for_counters(j) for j in picks()]
    print(f"[{m.now()}] rating batch: {len(todo)} examples", flush=True)
    for n, job in enumerate(todo, 1):
        name, out = m.make(job)
        print(f"[{m.now()}] ({n}/{len(todo)}) {name} {out}", flush=True)
        if out.startswith("BLOCKED"):
            sys.exit(3)


def ledger_by_candidate():
    idx = {}
    for line in open(os.path.join(m.MOD, ".hoi4-imagegen", "ledger.jsonl"), encoding="utf-8"):
        r = json.loads(line)
        if r["kind"] in ("generated", "edited"):
            idx[r["artifact_id"]] = r["data"]
    return idx


def review_one(target, family, v):
    fam = m.cli("families", family)["family"]
    keys = [q["key"] for q in fam.get("rubric_questions", [])]
    fail = set(v.get("fail", []))
    approved = v["verdict"] == "approved"
    answers = {k: (k not in fail) if not approved else True for k in keys}
    if not approved and not fail:
        answers = {k: False for k in keys}
    return m.call("hoi4.image_review", {"target": target, "verdict": v["verdict"], "reviewer": "claude-imagegen-docs (agent)", "answers": answers, "notes": v.get("notes", "")})


def cmd_review(args):
    verdicts = json.load(open(args[0], encoding="utf-8"))
    for key, v in verdicts.items():
        fam, asset = key.split("/")
        rec = m.load(fam, asset)
        if rec and rec.get("processed"):
            m.call("hoi4.image_preview", {"target": rec["processed"]})
            rec["review"] = review_one(rec["processed"], fam, v)
            rec["verdict"] = {"verdict": "pass" if v["verdict"] == "approved" else "fail", "reason": v.get("notes", ""), "by": "claude-imagegen-docs (agent)", "at": m.now(), "attempt": rec.get("attempt", 1)}
            rec["my_verdict"] = {"verdict": v.get("level"), "notes": v.get("notes", ""), "judged_against": "vanilla references of the family, native size and enlarged"}
            m.save(fam, asset, rec)
            print(key, v["verdict"], (rec["review"].get("error") or {}).get("message", ""), flush=True)
        elif v.get("artifact"):
            # a handback example: no gallery record, review the processed artifact directly and keep the answer beside it
            m.call("hoi4.image_preview", {"target": v["artifact"]})
            r = review_one(v["artifact"], fam, v)
            path = os.path.join(m.WORK, "rating-reviews.json")
            store = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
            store[key] = {"review": r, "verdict": v}
            json.dump(store, open(path, "w", encoding="utf-8"), indent=1)
            print(key, v["verdict"], "(handback)", flush=True)


def theme_of(brief):
    nation = next((n for n in NATIONS if n.lower() in brief.lower()), None)
    return nation or "unnamed setting"


def save_pair(src, folder, base):
    img = Image.open(src).convert("RGBA")
    native = os.path.join(folder, base + ".png")
    img.save(native)
    img.resize((img.width * 4, img.height * 4), Image.NEAREST).save(os.path.join(folder, base + "_4x.png"))
    return base + ".png", base + "_4x.png", list(img.size)


def vanilla_ref(folder, family, refs):
    out = os.path.join(folder, f"{family}_vanilla.png")
    if os.path.exists(out):
        return os.path.basename(out)
    for ref in refs or []:
        if ref.get("origin") in ("kit_shelf", "installed_game") and ref.get("role", "style_anchor") == "style_anchor":
            src = os.path.join(m.MOD, ref["path"])
            if os.path.exists(src):
                Image.open(src).convert("RGBA").save(out)
                return os.path.basename(out)
    return None


def validate_summary(v):
    res = ((v or {}).get("results") or [{}])[0]
    codes = sorted({f["code"] for f in res.get("findings", []) if f.get("level") in ("error", "warning")})
    return {"status": res.get("status") or ("pass" if (v or {}).get("failed") == 0 else "fail"), "codes": codes}


def review_summary(r):
    if not r:
        return {"verdict": "not reviewed", "notes": ""}
    return {"verdict": r.get("verdict") or (r.get("error") or {}).get("code"), "notes": r.get("notes", ""), "approval_id": r.get("approval_id")}


def effort(data):
    u = (data or {}).get("usage") or {}
    return u.get("reasoning_effort"), u.get("reported_effort")


def cmd_export(args):
    folder = args[0]
    os.makedirs(folder, exist_ok=True)
    led = ledger_by_candidate()
    fams = {f["family"]: f for f in m.cli("families")["families"]}
    entries = []
    for job in picks():
        fam, asset = job["family"], job["asset"]
        rec = m.load(fam, asset)
        if not rec:
            continue
        if rec.get("history"):
            # the rating batch is the first attempt; a later gallery attempt (reworded brief, new build) does not replace it
            rec = rec["history"][0]
        gen = rec.get("generate") or {}
        cand = ((gen.get("generation") or {}).get("candidates") or [{}])[0]
        e = {"id": f"{fam}.{asset}", "family": fam, "group": fams[fam]["group"], "brief": job["brief"], "theme": theme_of(job["brief"]),
             "plan_id": (gen.get("plan") or {}).get("plan_id"), "build": rec.get("tool_version")}
        e["reasoning_effort"], e["reported_effort"] = effort(led.get(cand.get("candidate_id")))
        e["canvas"], e["frames"] = fams[fam]["canvas"], fams[fam]["frames"]
        if not rec.get("processed"):
            e["result"] = "no image: " + str(rec.get("error") or "generation failed")
            e["validate"] = {"status": "not run", "codes": []}
            e["review"] = {"verdict": "not reviewed", "notes": ""}
            entries.append(e)
            continue
        e["native_path"], e["enlarged_path"], e["native_size"] = save_pair(os.path.join(m.MOD, rec["process"]["path"]), folder, e["id"])
        e["validate"] = validate_summary(rec.get("validate"))
        e["review"] = review_summary(rec.get("review"))
        e["my_verdict"] = rec.get("my_verdict")
        e["vanilla_ref"] = vanilla_ref(folder, fam, cand.get("references"))
        e["candidate_id"] = cand.get("candidate_id")
        if fam == "building":
            e["second_frame"] = "ruined frame made by hoi4.image_edit of the first candidate"
        entries.append(e)
    reviews = json.load(open(os.path.join(m.WORK, "rating-reviews.json"), encoding="utf-8")) if os.path.exists(os.path.join(m.WORK, "rating-reviews.json")) else {}
    extra = args[1:]
    if extra:
        handback, ids = extra[0], extra[1:]
        briefs_by_id = {}
        for jf in ("extra-jobs6.json",):
            p = os.path.join(m.WORK, jf)
            if os.path.exists(p):
                briefs_by_id.update({j["id"]: j["brief"] for j in json.load(open(p, encoding="utf-8"))})
        for hid in ids:
            rp = os.path.join(handback, hid, "result.json")
            if not os.path.exists(rp):
                entries.append({"id": f"handback.{hid}", "family": hid, "theme": "logo" if "logo" in hid else "regeneration", "result": "no image: the request was refused or failed"})
                continue
            res = json.load(open(rp, encoding="utf-8"))
            for n, c in enumerate(res.get("candidates") or [res], 1):
                art = c.get("processed_artifact") or ""
                fam_of = "gui_texture" if "logo" in hid else hid
                data = led.get(c.get("candidate_id"))
                base = f"handback.{hid}" + (f"-v{n}" if len(res.get("candidates") or []) > 1 else "")
                e = {"id": base, "family": fam_of, "group": fams[fam_of]["group"], "theme": "logo" if "logo" in hid else "regeneration",
                     "brief": briefs_by_id.get(hid) or (data or {}).get("brief") or "", "plan_id": res.get("plan_id"), "build": res.get("tool_version"),
                     "canvas": fams[fam_of]["canvas"] if fams[fam_of]["canvas"][0] else [512, 512], "frames": fams[fam_of]["frames"]}
                e["reasoning_effort"], e["reported_effort"] = effort(data)
                e["native_path"], e["enlarged_path"], e["native_size"] = save_pair(c["processed_png"], folder, base)
                e["validate"] = {"status": "pass" if c.get("validation_failed") == 0 else "fail", "codes": []}
                rv = reviews.get(f"{fam_of}/{art}") or reviews.get(f"{hid}/{art}") or {}
                e["review"] = review_summary(rv.get("review")) if rv else {"verdict": "not reviewed", "notes": ""}
                if rv:
                    e["my_verdict"] = {"verdict": rv["verdict"].get("level"), "notes": rv["verdict"].get("notes", ""), "judged_against": "vanilla references of the family, native size and enlarged"}
                e["vanilla_ref"] = vanilla_ref(folder, fam_of, (data or {}).get("references"))
                e["candidate_id"] = c.get("candidate_id")
                entries.append(e)
    meta = {
        "about": "Rating batch 1: real hoi4-imagegen output on the ChatGPT login, nothing curated. Every generated image is listed, including failures.",
        "build": m.tool_version(),
        "created": m.now(),
        "examples": entries,
    }
    with open(os.path.join(folder, "manifest.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(meta, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print("exported", len(entries), "entries to", folder)


if __name__ == "__main__":
    {"gen": cmd_gen, "review": cmd_review, "export": cmd_export}[sys.argv[1]](sys.argv[2:])
