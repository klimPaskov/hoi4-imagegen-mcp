//! `hoi4.image_set`: every icon of one focus tree file, planned (and optionally generated) as one set.
//!
//! See `imagegen_core::set` for how the icons are found. Every icon gets a `focus` plan with the same style pack,
//! style anchors, provider and quality, so the set shares one style, and focuses that share a sprite share one asset,
//! so nothing is made twice. Plans are free. With `generate` each plan is generated under its own idempotency key
//! (`<idempotency_key>-<asset>`), so a rerun after an interruption repeats nothing that finished.

use imagegen_core::error::RetryClass;
use imagegen_core::export;
use imagegen_core::family::Family;
use imagegen_core::set::{self, FocusTreeSet};
use serde_json::{json, Value};

use crate::api::{GenerateParams, SetParams};

use super::{EResult, Engine, EngineError};

/// Vanilla national focus icons that every icon of a set sends as style anchors when the caller names none and no
/// style pack applies, so the whole set shares one frame family (a round dark plaque ringed by a gold laurel wreath) and
/// one finish instead of each icon copying a different automatic pick. A file the game folder lacks is skipped.
const SET_STYLE_ANCHORS: &[&str] =
    &["focus_AST_its_fight_work_or_perish", "focus_FIN_motti_tactics", "focus_JAP_uplift_ainu_revolution_theory", "focus_mex_privatisation"];

/// The feature slug of a set when the caller names none: the tree file's stem as snake_case.
fn slug_of(file: &str) -> String {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    let a = set::asset_from_sprite(stem);
    // a stem that starts with a digit (01_generic) gets "tree_" in front, as a slug must start with a letter
    match a.strip_prefix("icon_") {
        Some(rest) if !stem.starts_with(|c: char| c.is_ascii_alphabetic()) => format!("tree_{rest}"),
        _ => a,
    }
}

impl Engine {
    /// Plan, and with `generate` make, the icons of one focus tree file.
    pub fn set(&self, p: SetParams) -> EResult<Value> {
        let tree: FocusTreeSet = set::focus_tree_set(self.fs.root(), self.game_dir().as_deref(), &p.focus_tree)?;
        let feature_slug = p.feature_slug.clone().unwrap_or_else(|| slug_of(&tree.file));
        imagegen_core::naming::validate_snake("feature slug", &feature_slug)?;
        let generate = p.generate.unwrap_or(false);
        let key = p.idempotency_key.clone().unwrap_or_default();
        if generate && key.is_empty() {
            return Err(EngineError::invalid("generate needs an idempotency_key; each icon uses <idempotency_key>-<asset>"));
        }
        let only_missing = p.only_missing.unwrap_or(true);
        let chosen: Vec<&set::SetIcon> = tree.icons.iter().filter(|i| !only_missing || i.missing()).collect();
        if chosen.is_empty() {
            return Err(EngineError::new(
                "nothing_to_make",
                format!(
                    "{} names {} icons and every one has its sprite and texture; pass only_missing: false to plan them all again",
                    tree.file,
                    tree.icons.len()
                ),
                RetryClass::Terminal,
            ));
        }
        let profile = self.family_profile(Family::Focus)?.clone();
        // one frame family for the whole set: the caller's anchors, else a pack's, else a few vanilla national focus icons
        let anchors: Value = match &p.references {
            Some(r) if !r.is_empty() => json!(r),
            _ if self.choose_pack(p.style_pack.as_deref())?.is_some() => json!([]),
            _ => {
                let game = self.game_dir();
                let found: Vec<Value> = SET_STYLE_ANCHORS
                    .iter()
                    .filter_map(|n| game.as_ref().map(|g| g.join("gfx/interface/goals").join(format!("{n}.dds"))))
                    .filter(|path| path.is_file())
                    .map(|path| json!({"path": path.to_string_lossy(), "role": "style_anchor"}))
                    .collect();
                json!(found)
            }
        };
        let mut icons = Vec::new();
        let (mut total_usd, mut made, mut failed) = (0f64, 0usize, 0usize);
        // the dominant colour of each icon follows its subject, never the country, so the set has contrast
        let colours = set::colour_plan(&chosen);
        // the directly national focuses (a third of the set at most) may carry one national element
        let national = set::national_plan(&chosen);
        let national_plan = national.clone();
        let mut hues: Vec<(String, String)> = Vec::new();
        for ((icon, colour), national) in chosen.iter().zip(colours.iter()).zip(national.iter().copied()) {
            let mut req = json!({
                "family": "focus",
                "feature_slug": feature_slug,
                "asset": icon.asset,
                "brief": set::icon_brief_coloured(icon, chosen.len(), colour, national),
                "references": anchors,
            });
            for (k, v) in [("provider", &p.provider), ("model", &p.model), ("quality", &p.quality), ("style_pack", &p.style_pack)] {
                if let Some(v) = v {
                    req[k] = json!(v);
                }
            }
            let installs_as = export::final_files(&profile, profile.canvas, &feature_slug, &icon.asset)
                .ok()
                .and_then(|files| export::sprite_entries(&profile, &feature_slug, &icon.asset, &files, None).ok().flatten())
                .and_then(|(_, entries)| entries.first().map(|e| e.name.clone()));
            let mut row = json!({
                "planned_colour": colour,
                "national_element": national,
                "sprite": icon.sprite,
                "asset": icon.asset,
                "focuses": icon.focuses,
                "names": icon.names,
                "missing": icon.missing(),
                "installs_as": installs_as,
                "tree_names_it": installs_as.as_deref().is_some_and(|s| s == icon.sprite),
            });
            let plan = match serde_json::from_value(req).map_err(|e| EngineError::invalid(e.to_string())).and_then(|r| self.plan(r)) {
                Ok(plan) => plan,
                Err(e) => {
                    failed += 1;
                    row["error"] = json!({"code": e.code, "message": e.message});
                    icons.push(row);
                    continue;
                }
            };
            total_usd += plan.estimate.usd;
            row["plan_id"] = json!(plan.plan_id);
            row["estimate_usd"] = json!(plan.estimate.usd);
            if generate {
                let g =
                    GenerateParams { plan_id: plan.plan_id.clone(), idempotency_key: format!("{key}-{}", icon.asset), approve_cost: p.approve_cost };
                match self.generate(g) {
                    Ok(v) => {
                        made += 1;
                        let candidates = v.get("candidates").cloned().unwrap_or(v);
                        if let Some(hue) = candidates
                            .get(0)
                            .and_then(|c| c.get("path"))
                            .and_then(Value::as_str)
                            .and_then(|path| self.fs.read(path, 64 * 1024 * 1024).ok())
                            .and_then(|b| imagegen_core::pixels::decode(&b, None, imagegen_core::pixels::DecodeLimits::default()).ok())
                        {
                            let hue = set::dominant_hue(&hue.0);
                            row["dominant_hue"] = json!(hue);
                            hues.push((icon.asset.clone(), hue));
                        }
                        row["generated"] = candidates;
                    }
                    Err(e) => {
                        failed += 1;
                        row["error"] = json!({"code": e.code, "message": e.message});
                    }
                }
            }
            icons.push(row);
        }
        let shared: Vec<&set::SetIcon> = chosen.iter().copied().filter(|i| i.focuses.len() > 1).collect();
        Ok(json!({
            "focus_tree": tree.file,
            "feature_slug": feature_slug,
            "icons_in_tree": tree.icons.len(),
            "planned": icons.len() - icons.iter().filter(|i| i.get("plan_id").is_none()).count(),
            "generated": made,
            "failed": failed,
            "skipped_present": tree.icons.len() - chosen.len(),
            "dynamic": tree.dynamic,
            "shared_sprites": shared.iter().map(|i| json!({"sprite": i.sprite, "focuses": i.focuses})).collect::<Vec<_>>(),
            "estimate_usd": (total_usd * 10000.0).round() / 10000.0,
            "style": {"style_pack": p.style_pack, "references": anchors, "provider": p.provider, "model": p.model, "quality": p.quality},
            "national_repeats": set::national_repeats(&national_plan),
            "variety": if generate { json!(set::judge_variety(&hues)) } else { Value::Null },
            "icons": icons,
            "next": if generate {
                "Process, validate, preview and review each candidate as usual. Where installs_as differs from the sprite the tree names, point the focus's icon at installs_as (or keep the tree's name with an alias in your .gfx). Read variety: when the verdict is too_alike, make the icons in its largest bucket again."
            } else {
                "The plans are free. For one shared look, generate one icon first, approve it, and pass it as a style_anchor reference (or a style_pack) when you run the set with generate: true and an idempotency_key."
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_slug_is_the_files_stem() {
        assert_eq!(super::slug_of("common/national_focus/GER_tree.txt"), "ger_tree");
        assert_eq!(super::slug_of("common/national_focus/01_generic.txt"), "tree_01_generic");
    }
}
