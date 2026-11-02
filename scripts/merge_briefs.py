#!/usr/bin/env python3
"""Merge the brief files written for the big gallery into scripts/gallery_briefs.json and check them.

  merge_briefs.py <briefs_A.json> [<briefs_B.json> ...]

Checks the counts (24 for the big families, 12 for the rest), unique slugs, word counts, forbidden words and the
number of style tags per family. Prints every problem, writes the merged file only when there are none.
"""
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIG = {"focus", "idea_spirit", "idea_concern", "report_event", "news_event", "decision", "technology", "leader_portrait", "flag", "counter_large", "counter_small"}
ALL = [
    "focus", "idea_spirit", "idea_concern", "officer_corps_spirit", "decision", "decision_category_icon", "state_modifier", "special_project_icon",
    "raid", "mio_department", "mio_trait", "mio_policy", "bop_side", "medal", "agency_upgrade", "operation_icon", "commander_trait", "personal_trait",
    "doctrine", "report_event", "news_event", "decision_category_picture", "operation_picture", "technology", "achievement", "super_event",
    "leader_portrait", "commander_portrait", "operative_portrait", "scientist_portrait", "advisor_card", "flag", "building", "faction_logo",
    "agency_logo", "counter_large", "counter_small", "division_emblem", "modifier_stat_icon", "gui_texture",
]
FORBIDDEN = re.compile(r"swastika|nazi|hitler|stalin|mussolini|churchill|roosevelt|franklin|mao zedong|hirohito|\bhttps?://", re.I)


def main(paths):
    merged = {}
    for p in paths:
        merged.update(json.load(open(p, encoding="utf-8")))
    problems = []
    for fam in ALL:
        items = merged.get(fam)
        want = 24 if fam in BIG else 12
        if not items:
            problems.append(f"{fam}: missing")
            continue
        if len(items) != want:
            problems.append(f"{fam}: {len(items)} examples, want {want}")
        slugs = [i["asset"] for i in items]
        if len(set(slugs)) != len(slugs):
            problems.append(f"{fam}: duplicate slugs")
        styled = sum(1 for i in items if i.get("style"))
        if fam not in ("counter_large", "counter_small", "division_emblem", "modifier_stat_icon") and styled != want // 6:
            problems.append(f"{fam}: {styled} styled, want {want // 6}")
        for i in items:
            if not re.fullmatch(r"[A-Za-z0-9_]{3,28}", i["asset"]):
                problems.append(f"{fam}/{i['asset']}: bad slug")
            if fam == "flag" and not re.fullmatch(r"[A-Z]{3}_[a-z]+", i["asset"]):
                problems.append(f"{fam}/{i['asset']}: flag slug must be TAG_ideology")
            n = len(i["brief"].split())
            if not 10 <= n <= 50:
                problems.append(f"{fam}/{i['asset']}: {n} words")
            if FORBIDDEN.search(i["brief"]):
                problems.append(f"{fam}/{i['asset']}: forbidden word")
            if i.get("style") not in (None, "linocut", "gouache", "ink"):
                problems.append(f"{fam}/{i['asset']}: bad style {i.get('style')}")
    extra = set(merged) - set(ALL)
    if extra:
        problems.append("unknown families: " + ", ".join(sorted(extra)))
    if problems:
        print("\n".join(problems))
        return 1
    out = os.path.join(ROOT, "scripts", "gallery_briefs.json")
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        json.dump({k: merged[k] for k in ALL}, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print("wrote", out, sum(len(v) for v in merged.values()), "briefs")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
