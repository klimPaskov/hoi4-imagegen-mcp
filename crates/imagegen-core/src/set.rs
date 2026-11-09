//! Set mode: every icon one focus tree file needs, planned as one set.
//!
//! A focus tree names its icons one focus at a time, and making them one by one drifts in style and repeats
//! subjects. [`focus_tree_set`] reads the tree through the shared HOI4 engine (the same index and content trace as the
//! audit), keeps the `icon` of every focus in the one file, and groups the focuses by the sprite they show: several
//! focuses that share a sprite share one asset, so nothing is made twice. Each icon carries what the game knows about
//! it (whether the sprite is defined and its texture exists) and what the brief can say about it: the focus ids and
//! their English names and descriptions from the mod's localisation.
//!
//! The engine side (`hoi4.image_set`) plans one `focus` asset per icon with the same style pack and style anchors,
//! and can generate them. Nothing here writes to the project or the game.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hoi4_engine::sprite_audit::{Allowlist, SpriteReport};
use hoi4_engine::sprite_trace::TraceOptions;
use hoi4_engine::{Workspace, WorkspaceOptions};
use serde::Serialize;

use crate::error::{Error, Result};

/// Most icons one set holds.
pub const MAX_ICONS: usize = 300;
/// Longest localised text quoted in a brief.
const MAX_TEXT: usize = 300;
/// Localisation files read at most.
const MAX_LOC_FILES: usize = 2000;

/// One icon of a set: a sprite and the focuses that show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SetIcon {
    /// The sprite the focuses name, as written.
    pub sprite: String,
    /// The asset name planned for it (snake_case, unique within the set).
    pub asset: String,
    /// Focus ids that show this sprite, in file order.
    pub focuses: Vec<String>,
    /// Their English names, where the mod's localisation has them.
    pub names: Vec<String>,
    /// Their English descriptions (`<id>_desc`), where present.
    pub descriptions: Vec<String>,
    /// True when a loaded `.gfx` file defines the sprite.
    pub defined: bool,
    /// The texture the sprite points at, if defined.
    pub texture: Option<String>,
    /// True when that texture exists in the mod or the game.
    pub texture_exists: bool,
}

impl SetIcon {
    /// True when the game would show no art: the sprite is undefined or its texture is missing.
    pub fn missing(&self) -> bool {
        !self.defined || !self.texture_exists
    }
}

/// The icons of one focus tree file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FocusTreeSet {
    /// The file, relative to the mod.
    pub file: String,
    /// Focus uses that could not be resolved (an icon built at run time), as `focus:<id>`.
    pub dynamic: Vec<String>,
    /// The icons, in the order the file first names them.
    pub icons: Vec<SetIcon>,
}

/// An asset name from a sprite name: `GFX_focus_GER_rearm` and `GFX_goal_rearm` lose their prefixes, the rest is
/// lower-cased snake_case. Never empty: a name with no letter becomes `icon`.
pub fn asset_from_sprite(sprite: &str) -> String {
    let lower = sprite.to_ascii_lowercase();
    let mut rest = lower.strip_prefix("gfx_").unwrap_or(&lower);
    for p in ["focus_", "goal_"] {
        if let Some(r) = rest.strip_prefix(p) {
            rest = r;
            break;
        }
    }
    let mut out = String::new();
    for c in rest.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let mut out = out.trim_end_matches('_').to_string();
    if !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out = format!("icon_{out}").trim_end_matches('_').to_string();
    }
    out.truncate(crate::naming::MAX_NAME_LEN);
    out.trim_end_matches('_').to_string()
}

fn walk_yml(dir: &Path, out: &mut Vec<std::path::PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= MAX_LOC_FILES {
            return;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() && depth < 4 {
            walk_yml(&e.path(), out, depth + 1);
        } else if ft.is_file() && e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".yml") {
            out.push(e.path());
        }
    }
}

/// English localisation of `keys` from the mod's `localisation/english` folder (and `localisation/` files named
/// `*_l_english.yml`). Formatting codes are stripped.
pub fn english_texts(root: &Path, keys: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut files = Vec::new();
    walk_yml(&root.join("localisation"), &mut files, 0);
    let mut out = BTreeMap::new();
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let in_english = f.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("english"));
        if !in_english && !name.contains("l_english") {
            continue;
        }
        let Ok(bytes) = crate::fsio::read_limited(&f, 8 * 1024 * 1024) else { continue };
        let loc = hoi4_syntax::localisation::LocFile::parse(bytes);
        for e in &loc.entries {
            if keys.contains(&e.key) && !out.contains_key(&e.key) {
                out.insert(e.key.clone(), clean_text(&loc.value(e)));
            }
        }
    }
    out
}

/// Localised text without colour codes (`§Y`, `§!`), icons (`£x`) and scripted parts (`[...]`, `$...$`).
pub fn clean_text(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    let mut skip_to: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(end) = skip_to {
            if c == end {
                skip_to = None;
            }
            continue;
        }
        match c {
            '§' => {
                chars.next();
            }
            '£' => {
                while chars.peek().is_some_and(|c| c.is_alphanumeric() || *c == '_') {
                    chars.next();
                }
            }
            '[' => skip_to = Some(']'),
            '$' => skip_to = Some('$'),
            '\\' if chars.peek() == Some(&'n') => {
                chars.next();
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > MAX_TEXT {
        format!("{}...", joined.chars().take(MAX_TEXT).collect::<String>())
    } else {
        joined
    }
}

/// The icons of the focus tree in `file` (relative to `root`). Reads the mod and, when given, the game; writes nothing.
pub fn focus_tree_set(root: &Path, game: Option<&Path>, file: &str) -> Result<FocusTreeSet> {
    let file = file.replace('\\', "/").trim_start_matches("./").to_string();
    crate::fsio::check_relative(&file)?;
    if !root.join(&file).is_file() {
        return Err(Error::NotFound(format!("{file} (the focus tree file, relative to the mod)")));
    }
    let ws = Workspace::load(&WorkspaceOptions {
        game: game.map(Path::to_path_buf),
        target: Some(root.to_path_buf()),
        include_dlc: game.is_some(),
        ..Default::default()
    });
    let target = ws.target_layer().map(|i| ws.layers[i].name.clone());
    let (_, report) = SpriteReport::run(&ws, &Allowlist::default(), &TraceOptions::default());
    let mut icons: Vec<SetIcon> = Vec::new();
    let mut dynamic = Vec::new();
    for u in &report.uses {
        if u.content_type != "focus.icon" || !u.path.eq_ignore_ascii_case(&file) || target.as_deref().is_some_and(|t| t != u.layer) {
            continue;
        }
        let focus = u.subject.as_deref().and_then(|s| s.strip_prefix("focus:")).unwrap_or("").to_string();
        if u.dynamic || u.sprite.is_empty() {
            dynamic.push(format!("focus:{focus}"));
            continue;
        }
        let texture_exists = u.texture.as_ref().is_some_and(|t| t.exists);
        match icons.iter_mut().find(|i| i.sprite.eq_ignore_ascii_case(&u.sprite)) {
            Some(i) => {
                if !focus.is_empty() && !i.focuses.contains(&focus) {
                    i.focuses.push(focus);
                }
            }
            None => icons.push(SetIcon {
                sprite: u.sprite.clone(),
                asset: String::new(),
                focuses: if focus.is_empty() { vec![] } else { vec![focus] },
                names: Vec::new(),
                descriptions: Vec::new(),
                defined: u.defined,
                texture: u.texture.as_ref().map(|t| t.path.clone()),
                texture_exists,
            }),
        }
    }
    if icons.len() > MAX_ICONS {
        return Err(Error::Invalid(format!("{file} names {} icons; a set holds at most {MAX_ICONS}", icons.len())));
    }
    // asset names, unique within the set
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for i in &mut icons {
        let base = asset_from_sprite(&i.sprite);
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        taken.insert(name.clone());
        i.asset = name;
    }
    let keys: BTreeSet<String> = icons.iter().flat_map(|i| i.focuses.iter().flat_map(|f| [f.clone(), format!("{f}_desc")])).collect();
    let texts = english_texts(root, &keys);
    for i in &mut icons {
        i.names = i.focuses.iter().filter_map(|f| texts.get(f).cloned()).filter(|t| !t.is_empty()).collect();
        i.descriptions = i.focuses.iter().filter_map(|f| texts.get(&format!("{f}_desc")).cloned()).filter(|t| !t.is_empty()).collect();
    }
    Ok(FocusTreeSet { file, dynamic, icons })
}

/// The brief of one icon: what the focuses are called and say, and that the icon is one of a set with distinct
/// subjects.
pub fn icon_brief(icon: &SetIcon, set_size: usize) -> String {
    let named = if icon.names.is_empty() { icon.focuses.join(", ") } else { icon.names.join("; ") };
    let mut brief = format!("The icon of the national focus {named}");
    if icon.names.is_empty() {
        brief.push_str(" (a focus id: read it as the focus's subject)");
    }
    brief.push('.');
    if let Some(d) = icon.descriptions.first() {
        brief.push_str(&format!(" The focus says: \"{d}\""));
    }
    if set_size > 1 {
        brief.push_str(&format!(
            " One of {set_size} icons of the same focus tree, made as one set: show this focus's own subject, not a generic emblem the other focuses could share."
        ));
    }
    brief
}

/// What gives an icon its dominant colour: the subject words in its focus names and descriptions, checked in order.
/// The country never picks a colour: a tree about one nation would otherwise paint every icon in that nation's
/// colours and symbols and leave no contrast between icons.
const SUBJECT_COLOURS: &[(&[&str], &str)] = &[
    (&["navy", "naval", "fleet", "ship", "sea", "harbour", "harbor", "port", "submarine", "convoy", "maritime"], "navy and sea blue"),
    (&["air", "aircraft", "fighter", "bomber", "pilot", "airfield", "airborne", "paratroop"], "pale sky blue and silver"),
    (
        &["army", "armies", "tank", "armour", "armor", "infantry", "conscript", "division", "garrison", "artillery", "military", "rearm"],
        "steel grey and khaki",
    ),
    (&["peat", "turf", "bog"], "earth brown and tan"),
    (
        &[
            "industry",
            "industrial",
            "factory",
            "factories",
            "production",
            "mine",
            "mining",
            "coal",
            "oil",
            "fuel",
            "steel",
            "machine",
            "construction",
            "infrastructure",
        ],
        "brass, amber and smoke grey",
    ),
    (&["farm", "farmer", "agriculture", "agricultural", "harvest", "peasant", "food", "forest", "rural"], "green and golden wheat"),
    (&["science", "research", "technology", "scientific", "university", "radio", "nuclear", "scientist", "engineer"], "pale blue and white"),
    (
        &["church", "religion", "faith", "culture", "cultural", "art", "revival", "heritage", "tradition", "monarch", "crown", "king"],
        "cream and gold with deep red",
    ),
    (
        &["revolt", "revolution", "rebel", "rebellion", "protest", "strike", "unrest", "uprising", "communis", "workers", "terror"],
        "dark red and black",
    ),
    (&["spy", "spies", "intelligence", "espionage", "secret", "police", "surveillance", "sabotage", "propaganda"], "plum and cream"),
    (&["trade", "economy", "economic", "bank", "money", "finance", "tax", "debt", "loan", "invest"], "gold and dark teal"),
    (
        &["diplomacy", "diplomatic", "treaty", "treaties", "alliance", "negotiation", "union", "peace", "neutral", "neutrality", "embassy"],
        "cream and soft grey",
    ),
];
/// Colours handed to icons no subject word places, in turn, so neighbours differ.
const SPARE_COLOURS: &[&str] = &[
    "ochre and sand",
    "deep teal and white",
    "dark violet and plum",
    "brick red and rust",
    "slate blue and silver",
    "olive and khaki",
    "warm cream and charcoal",
    "burnt orange and dark brown",
];

/// Colours whose main hue is green. A set may have at most one or two such icons (a sixth of it, at least one), and
/// only where the subject is green itself.
const GREEN_COLOURS: &[&str] = &["green and golden wheat"];
/// Palettes that are dark overall: two neighbouring icons never both take one.
const DARK_COLOURS: &[&str] = &["dark red and black", "burnt orange and dark brown", "dark violet and plum"];
/// Words that make a focus directly national: politics, the army and the nation's own identity. Only such icons
/// may carry the country's emblem or colours, and at most a third of a set does.
const NATIONAL_WORDS: &[&str] = &[
    "flag",
    "anthem",
    "identity",
    "independence",
    "sovereignty",
    "republic",
    "constitution",
    "unification",
    "reunification",
    "nationalism",
    "patriot",
    "patriotic",
    "homeland",
    "nation",
    "national",
    "army",
    "armies",
    "military",
    "defence",
    "defense",
    "conscript",
    "neutrality",
    "political",
    "politics",
    "government",
    "party",
    "election",
    "parliament",
    "monarchy",
    "crown",
    "culture",
    "cultural",
    "heritage",
    "language",
    "tradition",
];
/// What to draw for a subject that models tend to get wrong, by a word in the focus name (the first match counts).
const SUBJECT_HINTS: &[(&[&str], &str)] = &[
    (
        &["peat", "turf", "bog"],
        "Draw peat cutting: a brown bog, a turf-cutting spade and neat stacks of cut turf. No coal, no mine cart, no factory.",
    ),
    (
        &["spy", "spies", "intelligence", "espionage"],
        "Draw a trench-coated figure in a fedora, or a magnifying glass over a sealed envelope. Not a wallet or a notebook, no machinery.",
    ),
    (&["neutrality", "neutral", "peace"], "Draw a dove with an olive branch, or a closed gate. Not a handshake."),
    (&["treaty", "treaties", "diplomacy", "alliance"], "Draw one handshake, nothing else."),
    (&["trade", "economy", "bank", "finance"], "Draw stacked crates beside a pair of scales, or a cargo ship. Not a handshake."),
    (
        &["unrest", "protest", "strike", "revolt", "rebel", "uprising", "riot"],
        "Draw a crowd of raised fists before a barricade fire, or a torn banner. Not a prison gate or a jail.",
    ),
    (&["industry", "industrial", "factory", "factories"], "Draw one factory with one chimney, nothing else."),
    (&["science", "research", "scientific", "university"], "Draw one microscope or one flask, nothing else."),
];

/// True when the focus is directly national (by its name): politics, the army, culture or the nation's own identity.
pub fn is_national_focus(icon: &SetIcon) -> bool {
    let names = icon.names.join(" ").to_lowercase();
    NATIONAL_WORDS.iter().any(|w| has_word(&names, w))
}

/// Which icons of a set may carry one national element: the directly national focuses, at most a third of the set
/// (rounded up). Where more qualify, the earlier ones in the file win.
pub fn national_plan(icons: &[&SetIcon]) -> Vec<bool> {
    let cap = icons.len().div_ceil(3);
    let mut left = cap;
    icons
        .iter()
        .map(|i| {
            let yes = left > 0 && is_national_focus(i);
            if yes {
                left -= 1;
            }
            yes
        })
        .collect()
}

/// True when a word of the text is `word` or a simple inflection of it (`navy`, `navies`, `industrial` for
/// `industry` is a word of its own in the table, so it is listed there).
fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric()).any(|t| {
        t == word
            || ["s", "es", "ed", "ing", "al"].iter().any(|suf| t.strip_suffix(suf) == Some(word))
            || t.strip_suffix('s').and_then(|x| x.strip_suffix("ie")).is_some_and(|x| format!("{x}y") == word)
    })
}

/// The dominant colour planned for each icon of a set, in order. A subject word picks it, except that no colour
/// goes to more than a quarter of the set (rounded up, at least two): later icons that would exceed it take the next
/// spare colour. Icons no word places take the spare colours in turn.
pub fn colour_plan(icons: &[&SetIcon]) -> Vec<&'static str> {
    let cap = icons.len().div_ceil(4).max(2);
    let green_cap = (icons.len() / 6).max(1);
    let mut used: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut greens = 0usize;
    let mut previous: Option<&'static str> = None;
    let mut spare = 0usize;
    let mut out = Vec::with_capacity(icons.len());
    for icon in icons {
        // the focus name places the icon first; the description only when the name names no subject
        let names = icon.names.join(" ").to_lowercase();
        let descriptions = icon.descriptions.join(" ").to_lowercase();
        let place = |text: &str| SUBJECT_COLOURS.iter().find(|(words, _)| words.iter().any(|w| has_word(text, w))).map(|(_, c)| *c);
        let allowed = |c: &&'static str| {
            used.get(c).copied().unwrap_or(0) < cap
                && (!GREEN_COLOURS.contains(c) || greens < green_cap)
                && !(previous.is_some_and(|p| DARK_COLOURS.contains(&p)) && DARK_COLOURS.contains(c))
        };
        let by_subject = place(&names).or_else(|| place(&descriptions)).filter(|c| allowed(c));
        let colour = by_subject.unwrap_or_else(|| {
            // the next spare colour that is still allowed
            for _ in 0..SPARE_COLOURS.len() {
                let c = SPARE_COLOURS[spare % SPARE_COLOURS.len()];
                spare += 1;
                if allowed(&c) {
                    return c;
                }
            }
            SPARE_COLOURS[spare % SPARE_COLOURS.len()]
        });
        *used.entry(colour).or_insert(0) += 1;
        if GREEN_COLOURS.contains(&colour) {
            greens += 1;
        }
        previous = Some(colour);
        out.push(colour);
    }
    out
}

/// The brief of one icon in a set: the vanilla national focus form, the colour planned for it (it follows the
/// subject, and green is never the main colour unless the plan says so), and the national element only where the
/// focus is directly national (`national`, one element), none elsewhere.
pub fn icon_brief_coloured(icon: &SetIcon, set_size: usize, colour: &str, national: bool) -> String {
    let mut brief = icon_brief(icon, set_size);
    brief.push_str(" Draw it as a Hearts of Iron IV national focus icon: one emblem in aged painterly metal and enamel, on a round or shaped plaque or in a metal frame with a laurel wreath or ribbons, as the reference icons do. One clear main subject.");
    brief.push_str(&format!(" Colour: this icon is mainly {colour}, as its subject calls for."));
    if !GREEN_COLOURS.contains(&colour) {
        brief.push_str(" Green is not a main colour of this icon.");
    }
    if national {
        brief.push_str(" This focus is directly national, so exactly one clear national element is welcome, large and readable: the country's flag or flag colours, its harp or its emblem, one of them only.");
    } else {
        brief.push_str(" No flags, tricolour ribbons, harps, roundels, shamrocks, Celtic knotwork or national colours as trim: this icon is coloured by its subject.");
    }
    let names = icon.names.join(" ").to_lowercase();
    if let Some((_, hint)) = SUBJECT_HINTS.iter().find(|(words, _)| words.iter().any(|w| has_word(&names, w))) {
        brief.push(' ');
        brief.push_str(hint);
    }
    brief
}

/// How alike the dominant colours of a generated set are.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Variety {
    /// `ok`, `too_alike` (over 45 percent of the icons share one coloured bucket, over 70 percent have no colour beyond metal, or the set spans fewer than three), `too_green` (more green icons than a sixth of the set, at least one) or `too_few` (fewer than five icons to judge).
    pub verdict: &'static str,
    /// Icons measured.
    pub icons: usize,
    /// Distinct dominant hues (30 degree buckets) among the icons that have one.
    pub distinct_hues: usize,
    /// The share of icons in the most common bucket.
    pub largest_share: f64,
    /// Icons per hue bucket, `metal` for those with no colour beyond metal.
    pub buckets: BTreeMap<String, Vec<String>>,
    /// Icons whose dominant hue is green (bucket `h3` to `h5`), and the most a set of this size may have.
    pub green_icons: Vec<String>,
    /// The most green icons a set of this size may have: a sixth of it, at least one.
    pub green_cap: usize,
    /// What to do, when the verdict is not `ok`.
    pub advice: Option<String>,
}

/// The dominant hue bucket of an image, in 12 buckets of 30 degrees (`h0` red, `h4` green, `h8` blue). Every HOI4
/// icon sits in a frame of brass, bronze or wood, so those warm low-saturation colours (hue 15 to 65 degrees,
/// saturation under 0.8) and every grey or dark pixel are left out: the bucket is the colour of the enamel, cloth and
/// scene. An icon with under 8 percent of its opaque pixels left is `metal`: nothing but metal.
pub fn dominant_hue(img: &crate::pixels::Rgba) -> String {
    let mut hist = [0f64; 12];
    let (mut opaque, mut coloured) = (0u64, 0u64);
    for p in img.data.chunks_exact(4) {
        if p[3] < 200 {
            continue;
        }
        opaque += 1;
        let (r, g, b) = (f64::from(p[0]) / 255.0, f64::from(p[1]) / 255.0, f64::from(p[2]) / 255.0);
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let d = mx - mn;
        let sat = d / mx.max(1e-6);
        if mx < 0.12 || sat < 0.25 {
            continue;
        }
        let h = if (mx - r).abs() < f64::EPSILON {
            ((g - b) / d).rem_euclid(6.0)
        } else if (mx - g).abs() < f64::EPSILON {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } * 60.0;
        if (15.0..=65.0).contains(&h) && sat < 0.8 {
            continue;
        }
        coloured += 1;
        hist[((h / 30.0) as usize) % 12] += sat * mx;
    }
    if opaque == 0 || (coloured as f64) < 0.08 * opaque as f64 {
        return "metal".into();
    }
    // a hue on a bucket edge counts a little for both neighbours
    let score = |a: usize| hist[a] + 0.5 * (hist[(a + 1) % 12] + hist[(a + 11) % 12]);
    let best = (0..12).max_by(|&a, &b| score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal));
    format!("h{}", best.unwrap_or(0))
}

/// Judge a set from `(icon name, dominant hue bucket)` pairs. Five or more icons are judged: the set is `too_alike`
/// when over 45 percent of them share one coloured bucket, when over 70 percent have no colour beyond metal (muted
/// brown, grey and brass icons are fine in a minority, but not as the whole set), or when the set spans fewer than three.
pub fn judge_variety(items: &[(String, String)]) -> Variety {
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, bucket) in items {
        buckets.entry(bucket.clone()).or_default().push(name.clone());
    }
    let n = items.len();
    let distinct_hues = buckets.keys().filter(|k| k.as_str() != "metal").count();
    let largest = buckets.iter().filter(|(k, _)| k.as_str() != "metal").map(|(_, v)| v.len()).max().unwrap_or(0);
    let metal_share = if n == 0 { 0.0 } else { buckets.get("metal").map_or(0, Vec::len) as f64 / n as f64 };
    let largest_share = if n == 0 { 0.0 } else { (largest as f64 / n as f64 * 1000.0).round() / 1000.0 };
    let green_icons: Vec<String> = ["h3", "h4", "h5"].iter().filter_map(|k| buckets.get(*k)).flatten().cloned().collect();
    let green_cap = (n / 6).max(1);
    let verdict = if n < 5 {
        "too_few"
    } else if largest_share > 0.45 || metal_share > 0.7 || buckets.len() < 3 {
        "too_alike"
    } else if green_icons.len() > green_cap {
        "too_green"
    } else {
        "ok"
    };
    let advice = match verdict {
        "too_alike" => Some("Too many icons share one dominant colour. Make the ones in the largest bucket again with a different planned colour (set icons carry one in their brief), or choose references with more varied colours.".to_string()),
        "too_green" => Some(format!("Too many icons are mainly green ({} of {n}, at most {green_cap}). Make the green ones again unless their subject is green itself (fields, forests).", green_icons.len())),
        _ => None,
    };
    Variety { verdict, icons: n, distinct_hues, largest_share, buckets, green_icons, green_cap, advice }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_drop_the_sprite_prefixes() {
        assert_eq!(asset_from_sprite("GFX_focus_GER_Rearm"), "ger_rearm");
        assert_eq!(asset_from_sprite("GFX_goal_generic_construct_civ_factory"), "generic_construct_civ_factory");
        assert_eq!(asset_from_sprite("GFX_focus_1936-plan"), "icon_1936_plan");
        assert_eq!(asset_from_sprite("GFX_"), "icon");
        crate::naming::validate_snake("asset", &asset_from_sprite("GFX_focus_A__B")).unwrap();
    }

    #[test]
    fn localised_text_loses_codes_and_scripted_parts() {
        assert_eq!(clean_text("§YRearm§! the [ROOT.GetName] army £pol_power now\\nfast"), "Rearm the army now fast");
        assert!(clean_text(&"x".repeat(400)).ends_with("..."));
    }

    #[test]
    fn a_brief_names_the_focus_and_the_set() {
        let icon = SetIcon {
            sprite: "GFX_focus_a".into(),
            asset: "a".into(),
            focuses: vec!["TAG_a".into()],
            names: vec!["Steel for the Navy".into()],
            descriptions: vec!["Build ships.".into()],
            defined: false,
            texture: None,
            texture_exists: false,
        };
        assert!(icon.missing());
        let b = icon_brief(&icon, 12);
        assert!(b.contains("Steel for the Navy") && b.contains("\"Build ships.\"") && b.contains("One of 12 icons"), "{b}");
    }

    fn icon_named(name: &str) -> SetIcon {
        SetIcon {
            sprite: format!("GFX_focus_{name}"),
            asset: name.to_lowercase().replace(' ', "_"),
            focuses: vec![name.into()],
            names: vec![name.into()],
            descriptions: vec![],
            defined: false,
            texture: None,
            texture_exists: false,
        }
    }

    #[test]
    fn colours_follow_the_subject_and_not_the_country() {
        let icons: Vec<SetIcon> =
            ["Irish Navy Expansion", "Irish Tank Corps", "Irish Air Force", "Irish Heavy Industry", "Irish Harvest", "Irish Nuclear Research"]
                .iter()
                .map(|n| icon_named(n))
                .collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        let plan = colour_plan(&refs);
        assert_eq!(plan[0], "navy and sea blue");
        assert_eq!(plan[1], "steel grey and khaki");
        assert_eq!(plan[2], "pale sky blue and silver");
        assert_eq!(plan[3], "brass, amber and smoke grey");
        assert_eq!(plan[4], "green and golden wheat");
        assert_eq!(plan[5], "pale blue and white");
        let b = icon_brief_coloured(&icons[1], 6, plan[1], false);
        assert!(
            b.contains("mainly steel grey and khaki") && b.contains("Green is not a main colour") && b.contains("No flags, tricolour ribbons"),
            "{b}"
        );
        let flag = SetIcon { names: vec!["The Flag of the Republic".into()], ..icon_named("flag") };
        assert!(
            is_national_focus(&flag)
                && icon_brief_coloured(&flag, 6, "cream and gold with deep red", true).contains("exactly one clear national element")
        );
        assert!(!is_national_focus(&icons[0]));
        let peat = SetIcon { names: vec!["Peat and Coal for Power".into()], ..icon_named("peat") };
        assert!(icon_brief_coloured(&peat, 6, "earth brown and tan", false).contains("No coal, no mine cart"));
    }

    #[test]
    fn no_colour_takes_more_than_a_quarter_of_a_set() {
        // twelve naval focuses: the cap is 3, the rest take spare colours
        let icons: Vec<SetIcon> = (0..12).map(|i| icon_named(&format!("Fleet {i}"))).collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        let plan = colour_plan(&refs);
        let navy = plan.iter().filter(|c| **c == "navy and sea blue").count();
        assert_eq!(navy, 3, "{plan:?}");
        let distinct: BTreeSet<_> = plan.iter().collect();
        assert!(distinct.len() >= 4, "{plan:?}");
        assert!(!has_word("repair the roads and coal seams", "air") && !has_word("coal seams", "sea"));
        assert!(has_word("the navies", "navy") && has_word("the fleets", "fleet"));
    }

    fn flat(rgb: [u8; 3]) -> crate::pixels::Rgba {
        let mut img = crate::pixels::Rgba::new(8, 8);
        for p in img.data.chunks_exact_mut(4) {
            p.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        img
    }

    #[test]
    fn dominant_hue_buckets_and_the_variety_verdict() {
        assert_eq!(dominant_hue(&flat([200, 30, 30])), "h0");
        assert_eq!(dominant_hue(&flat([30, 200, 30])), "h4");
        assert_eq!(dominant_hue(&flat([30, 30, 200])), "h8");
        assert_eq!(dominant_hue(&flat([120, 120, 125])), "metal");
        assert_eq!(dominant_hue(&flat([180, 140, 60])), "metal", "brass is the frame, not a colour");
        let greens: Vec<(String, String)> = (0..8).map(|i| (format!("i{i}"), dominant_hue(&flat([30, 190 + i as u8, 40])))).collect();
        let v = judge_variety(&greens);
        assert_eq!(v.verdict, "too_alike");
        assert!(v.advice.is_some() && v.largest_share > 0.9, "{v:?}");
        let mixed: Vec<(String, String)> = [[200, 30, 30], [30, 200, 30], [30, 30, 200], [200, 180, 30], [120, 30, 160], [120, 120, 125]]
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("i{i}"), dominant_hue(&flat(*c))))
            .collect();
        assert_eq!(judge_variety(&mixed).verdict, "ok");
        assert_eq!(judge_variety(&mixed[..3]).verdict, "too_few");
    }

    #[test]
    fn green_is_budgeted_and_dark_palettes_never_neighbour() {
        // twelve farming focuses: only two may be mainly green
        let icons: Vec<SetIcon> = (0..12).map(|i| icon_named(&format!("Farm {i}"))).collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        let plan = colour_plan(&refs);
        assert_eq!(plan.iter().filter(|c| GREEN_COLOURS.contains(c)).count(), 2, "{plan:?}");
        // unrest then a second dark subject: the second takes a lighter colour
        let pair = [icon_named("Border Unrest"), icon_named("Terror Cells")];
        let refs: Vec<&SetIcon> = pair.iter().collect();
        let plan = colour_plan(&refs);
        assert_eq!(plan[0], "dark red and black");
        assert!(!DARK_COLOURS.contains(&plan[1]), "{plan:?}");
        // a set with four green icons of twelve is too green
        let greens: Vec<(String, String)> =
            (0..12).map(|i| (format!("i{i}"), ["h4", "h4", "h4", "h4", "h0", "h1", "h6", "h7", "h8", "h9", "h10", "metal"][i].to_string())).collect();
        let v = judge_variety(&greens);
        assert_eq!((v.verdict, v.green_icons.len(), v.green_cap), ("too_green", 4, 2), "{v:?}");
    }

    #[test]
    fn national_symbols_go_to_the_national_focuses_and_at_most_a_third() {
        let names = [
            "Irish Neutrality",
            "Expand the Irish Army",
            "Build the Irish Navy",
            "Irish Air Corps",
            "Heavy Industry",
            "Peat and Coal",
            "Reform Agriculture",
            "Gaelic Cultural Revival",
            "Research",
            "Border Unrest",
            "Intelligence Service",
            "Atlantic Trade",
        ];
        let icons: Vec<SetIcon> = names.iter().map(|n| icon_named(n)).collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        let plan = national_plan(&refs);
        let chosen: Vec<&str> = names.iter().zip(&plan).filter(|(_, y)| **y).map(|(n, _)| *n).collect();
        assert_eq!(chosen, ["Irish Neutrality", "Expand the Irish Army", "Gaelic Cultural Revival"]);
        // twelve armies: only a third carry the emblem
        let armies: Vec<SetIcon> = (0..12).map(|i| icon_named(&format!("Army {i}"))).collect();
        let refs: Vec<&SetIcon> = armies.iter().collect();
        assert_eq!(national_plan(&refs).iter().filter(|y| **y).count(), 4);
        // hints keep the subjects apart: one handshake only
        let neutral = icon_brief_coloured(&icons[0], 12, "cream and soft grey", true);
        let trade = icon_brief_coloured(&icons[11], 12, "gold and dark teal", false);
        assert!(neutral.contains("Not a handshake") && trade.contains("Not a handshake") && !trade.contains("Draw one handshake"));
    }
}
