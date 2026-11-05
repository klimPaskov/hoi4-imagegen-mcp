#!/usr/bin/env python3
"""One building with its ruined second frame made from the first candidate, handed back with every file.

  make_building_pair.py <asset> "<brief>" "<refine instruction>" <handback folder>

hoi4-imagegen generates the intact building (v1, one plan), hoi4.image_edit refines THAT candidate with the instruction
(v2), and hoi4.image_process builds the strip with v2 as the second frame. The handback folder gets <asset>/ with the raw
v1 and v2, every processed file the family writes, and result.json (both plan ids, tool version, notes).
Environment as for make_gallery_big.py: GALLERY_MOD, HOI4_IMAGEGEN_BIN, GALLERY_RES, GALLERY_EXPECT_VERSION.
"""
import json
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_gallery_big as m  # noqa: E402


def cand_of(r):
    c = (r.get("generation") or {}).get("candidates") or (r.get("edit") or {}).get("candidates") or r.get("candidates") or []
    return c[0] if c else ({"candidate_id": r["candidate_id"], **r} if r.get("candidate_id") else None)


def main(asset, brief, instruction, handback, refs=None):
    m.check_build()
    out = os.path.join(handback, asset)
    os.makedirs(out, exist_ok=True)
    key = f"pair-{asset}"
    req = {"family": "building", "feature_slug": "gallery", "asset": asset, "brief": brief, "approve": True, "idempotency_key": key}
    if refs:
        # explicit style anchors (project-relative paths), for example vanilla landmark icons from the mod's shelf copy
        req["references"] = [{"path": p, "role": "style_anchor"} for p in refs.split(",")]
    r1 = m.call("hoi4.image_unit", req)
    c1 = cand_of(r1)
    if not c1:
        sys.exit("v1 failed: " + json.dumps(r1)[:400])
    r2 = m.call("hoi4.image_edit", {"candidate_id": c1["candidate_id"], "instruction": instruction, "idempotency_key": key + "-refine"})
    c2 = cand_of(r2)
    if not c2:
        sys.exit("refine failed: " + json.dumps(r2)[:400])
    p = m.call("hoi4.image_process", {"candidate_id": c1["candidate_id"], "second_frame_candidate": c2["candidate_id"]})
    if not p.get("artifact_id"):
        sys.exit("process failed: " + json.dumps(p)[:400])
    v = m.call("hoi4.image_validate", {"targets": [p["artifact_id"]]})
    files = {}
    for name, c in (("v1", c1), ("v2", c2)):
        src = os.path.join(m.MOD, c["path"])
        dst = os.path.join(out, f"{asset}-{name}-raw.png")
        shutil.copyfile(src, dst)
        files[name + "_raw"] = dst
    processed = [p["path"]] + [x["path"] if isinstance(x, dict) else x for x in p.get("companions", [])]
    files["processed"] = []
    for rel in processed:
        src = os.path.join(m.MOD, rel)
        if os.path.exists(src):
            dst = os.path.join(out, os.path.basename(rel))
            shutil.copyfile(src, dst)
            files["processed"].append(dst)
    res = {
        "asset": asset,
        "tool_version": m.tool_version(),
        "v1": {"candidate_id": c1["candidate_id"], "plan_id": (r1.get("plan") or {}).get("plan_id"), "note": c1.get("note"), "prompt_sha256": c1.get("prompt_sha256"),
               "references": [{"path": x.get("path"), "origin": x.get("origin"), "role": x.get("role"), "sha256": x.get("sha256")} for x in c1.get("references", [])]},
        "v2": {"candidate_id": c2["candidate_id"], "refined_from": c1["candidate_id"], "instruction": instruction, "plan_id": (r2.get("plan") or {}).get("plan_id"), "note": c2.get("note")},
        "processed_artifact": p["artifact_id"],
        "recipe": p.get("recipe"),
        "validation_failed": v.get("failed"),
        "files": files,
        "generated_at": m.now(),
    }
    json.dump(res, open(os.path.join(out, "result.json"), "w", encoding="utf-8"), indent=1)
    print("done", asset, res["v1"]["plan_id"], res["v2"]["plan_id"], flush=True)


if __name__ == "__main__":
    main(*sys.argv[1:6])
