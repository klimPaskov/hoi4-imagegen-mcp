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
    /// Why this is one of the tree's key focuses (its start, a branch hub, a choice between paths, the end of a
    /// branch), when it is one. Key focuses get an icon that stands out; the rest keep the quiet vanilla look.
    pub key: Option<String>,
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
    /// The country the tree belongs to, as its adjective (`Irish`): from the tag most focus ids start with and the
    /// `<TAG>_ADJ` localisation of the mod or the game. `None` when no tag or adjective is found; then no icon gets a
    /// national element, because a brief that says only "the country's flag" got another country's (2026-11-09).
    pub nation: Option<String>,
}

/// The tag most focus ids of a set start with (`IRE` in `IRE_neutrality`): three upper-case letters or digits shared
/// by more than half of the focuses.
pub fn tree_tag(icons: &[SetIcon]) -> Option<String> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    let mut total = 0usize;
    for f in icons.iter().flat_map(|i| i.focuses.iter()) {
        total += 1;
        if let Some((tag, _)) = f.split_once('_') {
            if tag.len() == 3 && tag.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
                *count.entry(tag).or_default() += 1;
            }
        }
    }
    count.into_iter().max_by_key(|(_, n)| *n).filter(|(_, n)| *n * 2 > total).map(|(t, _)| t.to_string())
}

/// The localisation keys that may hold a tag's adjective, in the order they are tried.
pub fn adjective_keys(tag: &str) -> Vec<String> {
    std::iter::once(format!("{tag}_ADJ"))
        .chain(["democratic", "neutrality", "fascism", "communism"].iter().map(|i| format!("{tag}_{i}_ADJ")))
        .collect()
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
                key: None,
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
    // the key focuses, from the tree's structure
    if let Ok(bytes) = crate::fsio::read_limited(&root.join(&file), 16 * 1024 * 1024) {
        mark_key_icons(&mut icons, &focus_structure(bytes));
    }
    // the country, so a national element names it
    let nation = tree_tag(&icons).and_then(|tag| {
        let keys: BTreeSet<String> = adjective_keys(&tag).into_iter().collect();
        let mut found = english_texts(root, &keys);
        if found.is_empty() {
            if let Some(g) = game {
                found = english_texts(g, &keys);
            }
        }
        adjective_keys(&tag).into_iter().find_map(|k| found.get(&k).cloned()).filter(|a| !a.is_empty())
    });
    Ok(FocusTreeSet { file, dynamic, icons, nation })
}

/// One focus of a tree file: its id, the focuses it requires and whether it excludes another (a choice of paths).
#[derive(Debug, Clone, PartialEq)]
pub struct FocusNode {
    /// The focus id.
    pub id: String,
    /// Every focus named in its `prerequisite` blocks.
    pub prerequisites: Vec<String>,
    /// True when it has a `mutually_exclusive` block.
    pub exclusive: bool,
}

/// The focuses of a tree file (`focus` and `shared_focus` blocks at any depth), with their links.
pub fn focus_structure(bytes: Vec<u8>) -> Vec<FocusNode> {
    use hoi4_syntax::{Block, Document};
    fn walk(doc: &Document, block: &Block, out: &mut Vec<FocusNode>, depth: usize) {
        if depth > 6 {
            return;
        }
        for e in block.entries() {
            let key = doc.key(e).to_ascii_lowercase();
            let Some(b) = e.value.as_block() else { continue };
            if key == "focus" || key == "shared_focus" {
                let mut node = FocusNode { id: String::new(), prerequisites: Vec::new(), exclusive: false };
                for f in b.entries() {
                    match doc.key(f).to_ascii_lowercase().as_str() {
                        "id" => {
                            if let Some(t) = f.value.as_scalar() {
                                node.id = doc.text(t).to_string();
                            }
                        }
                        "prerequisite" => {
                            if let Some(pb) = f.value.as_block() {
                                for p in pb.entries().filter(|p| doc.key(p).eq_ignore_ascii_case("focus")) {
                                    if let Some(t) = p.value.as_scalar() {
                                        node.prerequisites.push(doc.text(t).to_string());
                                    }
                                }
                            }
                        }
                        "mutually_exclusive" => node.exclusive = true,
                        _ => {}
                    }
                }
                if !node.id.is_empty() {
                    out.push(node);
                }
            } else {
                walk(doc, b, out, depth + 1);
            }
        }
    }
    let doc = Document::parse(bytes);
    let mut out = Vec::new();
    walk(&doc, &doc.root, &mut out, 0);
    out
}

/// How much each focus matters to its tree, and why: the tree's start (a root that opens a branch), a hub that opens
/// two or more branches, a choice between paths (mutually exclusive), the end of a long branch (nothing follows it, at
/// least three steps deep). Each counts 2, and a hub one more per branch beyond two (up to two more); a focus that
/// is none of these scores 0.
pub fn focus_weight(nodes: &[FocusNode]) -> BTreeMap<String, (u32, Vec<&'static str>)> {
    let mut dependents: BTreeMap<&str, usize> = BTreeMap::new();
    for n in nodes {
        for p in &n.prerequisites {
            *dependents.entry(p.as_str()).or_default() += 1;
        }
    }
    let by_id: BTreeMap<&str, &FocusNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    fn depth<'a>(id: &'a str, by_id: &BTreeMap<&'a str, &'a FocusNode>, seen: &mut Vec<&'a str>) -> usize {
        if seen.contains(&id) || seen.len() > 64 {
            return 0;
        }
        let Some(n) = by_id.get(id) else { return 0 };
        seen.push(id);
        let d = n.prerequisites.iter().map(|p| 1 + depth(p, by_id, seen)).max().unwrap_or(0);
        seen.pop();
        d
    }
    nodes
        .iter()
        .map(|n| {
            let deps = dependents.get(n.id.as_str()).copied().unwrap_or(0);
            let mut why = Vec::new();
            if n.prerequisites.is_empty() && deps >= 1 {
                why.push("the start of the tree");
            }
            if deps >= 2 {
                why.push("it opens several branches");
            }
            if n.exclusive {
                why.push("a choice between paths");
            }
            if deps == 0 && depth(&n.id, &by_id, &mut Vec::new()) >= 3 {
                why.push("the end of a long branch");
            }
            // a hub weighs more the more branches it opens (up to two more), so a big hub beats the end of a branch
            let hub_bonus = if deps >= 2 { (deps as u32 - 2).min(2) } else { 0 };
            (n.id.clone(), (2 * why.len() as u32 + hub_bonus, why))
        })
        .collect()
}

/// Mark the key icons of a set: about a quarter of it (at least one in a set of four or more), the icons whose focuses
/// weigh most in the tree ([`focus_weight`]); a directly national focus wins a tie. Icons whose focuses weigh nothing
/// are never key, so a flat list of focuses has none.
pub fn mark_key_icons(icons: &mut [SetIcon], nodes: &[FocusNode]) {
    let weights = focus_weight(nodes);
    let mut ranked: Vec<(usize, u32, Vec<&'static str>)> = icons
        .iter()
        .enumerate()
        .filter_map(|(i, icon)| {
            let best = icon.focuses.iter().filter_map(|f| weights.get(f)).max_by_key(|w| w.0)?;
            (best.0 >= 2).then(|| (i, best.0 + u32::from(is_national_focus(icon)), best.1.clone()))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let cap = if icons.len() >= 4 { icons.len().div_ceil(4) } else { 0 };
    for (i, _, why) in ranked.into_iter().take(cap) {
        icons[i].key = Some(why.join(", "));
    }
}

/// Frames for the key focuses, designed for the subject and free of the vanilla references: `(key, subject words,
/// what the brief says)`. The ones with no words suit any subject.
pub const STANDOUT_FRAMES: &[(&str, &[&str], &str)] = &[
    (
        "cogwheel",
        &["industry", "industrial", "factory", "factories", "production", "steel", "machine", "development"],
        "a heavy bronze cogwheel rim around the disc, with smoke and sparks rising behind it",
    ),
    (
        "anchor",
        &["navy", "naval", "fleet", "ship", "sea", "harbour", "maritime", "coastal", "submarine", "submarines", "flotilla", "flotillas"],
        "a coiled rope ring with a great anchor behind the disc",
    ),
    (
        "great_wings",
        &["air", "aircraft", "fighter", "bomber", "pilot", "corps"],
        "great spread bronze wings sweeping up from behind the disc, with a star at the top",
    ),
    (
        "crest",
        &["army", "armies", "military", "infantry", "soldier", "defence", "defense", "fortify", "fortress", "fortification"],
        "a heraldic shield over crossed rifles and a laurel spray, with a banner below (no crown)",
    ),
    (
        "wheat",
        &["farm", "farms", "agriculture", "harvest", "food", "rural"],
        "a wreath of golden wheat sheaves around the disc, with a sickle crossed behind it",
    ),
    (
        "torch_arch",
        &["culture", "cultural", "revival", "education", "science", "scientific", "research", "university", "language"],
        "a tall arched tablet with a lit torch rising behind it",
    ),
    (
        "chains",
        &["unrest", "revolt", "revolution", "rebel", "uprising", "strike", "independence", "freedom"],
        "broken chains wrapped around a cracked rim, with flames behind it",
    ),
    (
        "compass",
        &["trade", "economy", "diplomacy", "treaty", "atlantic", "export", "exports"],
        "a compass-rose star behind the disc, its points reaching past the rim",
    ),
    ("seal", &["spy", "intelligence", "secret", "espionage", "bureau"], "a large wax-seal medallion with ribbon tails"),
    (
        "linked_shields",
        &["union", "alliance", "cooperation", "pact", "league", "nordic", "federation", "together"],
        "a ring of small interlinked shields around the disc, joined by a chain",
    ),
    (
        "dam_rays",
        &["power", "hydroelectric", "electric", "electrification", "energy", "dam"],
        "a heavy bronze rim with lightning-bolt rays bursting out of it",
    ),
    ("sunburst", &[], "a sunburst of bronze rays behind the disc, framed by a laurel"),
    ("star_banners", &[], "a heraldic shield topped by a star, with banners hanging at its sides"),
    ("star", &[], "a large five-pointed star plaque with the subject set on it"),
    ("double_wreath", &[], "a double wreath of laurel and oak with two crossed banners behind the disc"),
    ("radiant_shield", &[], "a pointed heraldic shield with bronze rays fanning out behind it"),
];

/// The standout frame of each key icon (`None` for the others): the first unused frame whose words the focus names,
/// else the first unused one that suits any subject (never one made for another subject, so a union never gets a
/// cogwheel), so no two key icons share a frame where the list allows.
pub fn standout_plan(icons: &[&SetIcon]) -> Vec<Option<&'static str>> {
    let mut used: Vec<&str> = Vec::new();
    icons
        .iter()
        .map(|icon| {
            icon.key.as_ref()?;
            let names = icon.names.join(" ").to_lowercase();
            let pick = STANDOUT_FRAMES
                .iter()
                .find(|(k, words, _)| !used.contains(k) && words.iter().any(|w| has_word(&names, w)))
                .or_else(|| STANDOUT_FRAMES.iter().find(|(k, words, _)| !used.contains(k) && words.is_empty()))
                .map_or("sunburst", |f| f.0);
            used.push(pick);
            Some(pick)
        })
        .collect()
}

/// Focus words for which hands (a handshake, clasped hands) are the natural subject.
const HAND_WORDS: &[&str] =
    &["union", "alliance", "allies", "cooperation", "pact", "treaty", "treaties", "friendship", "agreement", "partnership", "solidarity"];

/// Which icon of a set may show hands: the first whose focus names an alliance, a union, a pact or the like, and no
/// other. A set of twenty drew clasped hands three times (a union, a cooperation and a welfare home, 2026-11-09).
pub fn hands_plan(icons: &[&SetIcon]) -> Vec<bool> {
    let mut given = false;
    icons
        .iter()
        .map(|icon| {
            let names = icon.names.join(" ").to_lowercase();
            let may = !given && HAND_WORDS.iter().any(|w| has_word(&names, w));
            given |= may;
            may
        })
        .collect()
}

/// The brief sentence for an icon that may not show hands.
pub const NO_HANDS: &str = " Show no hands: no handshake and no clasped, open or cupped hands (at most one icon of the set shows hands).";

/// The brief sentence of a key focus's icon.
pub fn standout_sentence(frame: &str, why: &str) -> String {
    let what = STANDOUT_FRAMES.iter().find(|(k, _, _)| *k == frame).map_or("a sunburst of bronze rays behind the disc", |f| f.2);
    format!(
        " This is one of the tree's key focuses ({why}), so its icon stands out from the ordinary ones. Frame: {what}. Design this frame yourself for this focus, bolder and more ornate than the plain medallions, and do not copy a frame from the reference images. Give it more presence: dramatic light, and a few vivid colour accents on the subject (this overrides the grey-subject rule), while it stays one compact emblem readable at native size."
    )
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
/// Colour families: two neighbouring icons never both take the same family (navy next to air, industry next to peat).
const COLOUR_FAMILIES: &[&[&str]] = &[
    &["navy and sea blue", "pale sky blue and silver", "pale blue and white", "slate blue and silver", "gold and dark teal", "deep teal and white"],
    &["brass, amber and smoke grey", "earth brown and tan", "burnt orange and dark brown", "ochre and sand", "brick red and rust"],
    &["steel grey and khaki", "olive and khaki", "warm cream and charcoal"],
];

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
        "Show peat cutting as a bog with neat stacks of cut turf, not coal and not a mine, and no spade or other tool.",
    ),
    (
        &["spy", "spies", "intelligence", "espionage"],
        "Show a closed folder with a lock and a key, or one eye in a keyhole, with nothing behind it. Not a detective, not a man in a hat or coat, no magnifying glass, not a wallet, no buildings or city.",
    ),
    (
        &["army", "armies", "military", "infantry", "conscript", "soldier", "soldiers"],
        "Show a helmet and rifle, or one soldier figure painted in grey like a vanilla statue, never a realistic crowd of soldiers.",
    ),
    (
        &["air", "aircraft", "fighter", "pilot", "airfield", "airborne"],
        "Show the aircraft as an emblem sitting inside its frame like the other icons of the set, not a loose cut-out floating on the background.",
    ),
    (&["neutrality", "neutral", "peace"], "Show a dove with an olive branch, scales or a closed gate, not a handshake."),
    (&["trade", "economy", "bank", "finance"], "Show crates, scales or a 1930s steam cargo ship (not a sailing ship), not a handshake."),
    (&["unrest", "protest", "strike", "revolt", "rebel", "uprising", "riot"], "Show raised fists, a burning barricade or a torn banner, not a prison gate."),
];

/// True when the focus is directly national (by its name): politics, the army, culture or the nation's own identity.
pub fn is_national_focus(icon: &SetIcon) -> bool {
    let names = icon.names.join(" ").to_lowercase();
    NATIONAL_WORDS.iter().any(|w| has_word(&names, w))
}

/// The kinds of national element an icon may carry, in the order they are handed out so that no two national icons of a
/// set use the same one (the country's flag in the scene, its emblem worked into the subject, its traditional cross or
/// knotwork, and last its colours as a ribbon or sash, so a set shows its flag colours once). Each is part of the icon,
/// never a logo. There is no landmark kind: a landscape behind the subject breaks vanilla's medallion, and the model
/// drew another country's landmark (Westminster behind an Irish intelligence folder, 2026-11-09).
pub const NATIONAL_ELEMENTS: &[(&str, &str)] = &[
    ("flag", "the country's flag flying as part of the scene (behind the subject, on a mast or on a pole)"),
    ("emblem", "the country's national emblem or instrument worked into the subject itself (carved, embroidered or painted on it)"),
    ("knotwork", "the country's traditional cross or knotwork worked into the frame or the subject"),
    ("sash", "the country's flag colours as a ribbon, sash or banner worked into the wreath or the frame"),
];

/// The frames a set hands out, as vanilla mixes them: `(key, what the brief says)`. Most are a round medallion with a
/// wreath; the rest vary the ornament or, now and then, the plaque's shape. All keep vanilla's thin rim.
pub const FRAMES: &[(&str, &str)] = &[
    ("laurel", "a round medallion with a gilded laurel wreath curving up both sides"),
    ("oak", "a round medallion with a dark bronze oak-leaf wreath"),
    ("plain", "a round medallion with a plain thin rim and no wreath, the subject breaking out of the rim at the top"),
    ("wings", "a round medallion with a pair of spread metal wings at its sides"),
    ("arms", "a round medallion with crossed rifles or sabres behind it"),
    ("shield", "a pointed heraldic shield in place of the disc, with the same thin rim"),
    ("diamond", "a diamond plaque in place of the disc, with the same thin rim"),
];
/// Target share of each frame over a set (wings and crossed arms only where the subject calls for them).
const FRAME_SHARES: &[(&str, f64)] = &[("laurel", 0.35), ("oak", 0.2), ("plain", 0.2), ("shield", 0.1), ("diamond", 0.08)];
/// Subjects that take their own ornament.
const FRAME_SUBJECTS: &[(&[&str], &str)] = &[
    (&["air", "aircraft", "fighter", "bomber", "pilot", "airfield", "airborne", "paratroop"], "wings"),
    (&["army", "armies", "infantry", "conscript", "garrison", "rearm", "soldier", "soldiers"], "arms"),
];

/// The frame of each icon of a set and whether it carries a short ribbon banner. Subjects that call for wings or
/// crossed arms get them; every other icon takes the frame furthest behind its share. Two neighbours never share a
/// frame, and every third icon has a ribbon banner, so the set varies the way vanilla does.
pub fn frame_plan(icons: &[&SetIcon]) -> Vec<(&'static str, bool)> {
    let mut out: Vec<(&'static str, bool)> = Vec::new();
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, icon) in icons.iter().enumerate() {
        let names = icon.names.join(" ").to_lowercase();
        let previous = out.last().map(|f| f.0);
        let own = FRAME_SUBJECTS.iter().find(|(words, _)| words.iter().any(|w| has_word(&names, w))).map(|f| f.1).filter(|f| Some(*f) != previous);
        let key = own.unwrap_or_else(|| {
            let mut best: Option<(&'static str, f64)> = None;
            for (k, share) in FRAME_SHARES.iter().filter(|(k, _)| Some(*k) != previous) {
                let behind = share * (i + 1) as f64 - *count.get(k).unwrap_or(&0) as f64;
                if best.is_none_or(|b| behind > b.1 + 1e-9) {
                    best = Some((k, behind));
                }
            }
            best.map_or("laurel", |b| b.0)
        });
        *count.entry(key).or_default() += 1;
        out.push((key, i % 3 == 1));
    }
    out
}

/// The brief sentence for a planned frame.
pub fn frame_sentence(key: &str, ribbon: bool) -> String {
    let what = FRAMES.iter().find(|(k, _)| *k == key).map_or("a round medallion with a laurel wreath", |f| f.1);
    let ribbon = if ribbon { ", with a short plain ribbon banner across the bottom (no writing)" } else { ", with no ribbon banner" };
    format!(" Frame: {what}{ribbon}.")
}

/// Which icons of a set may carry one national element, and which kind: the directly national focuses, at most a third
/// of the set (rounded up), each a different kind. Where more qualify, the earlier ones in the file win. More national
/// icons than kinds would repeat a kind, which [`national_repeats`] reports.
pub fn national_plan(icons: &[&SetIcon]) -> Vec<Option<&'static str>> {
    let cap = icons.len().div_ceil(3);
    let mut given = 0usize;
    icons
        .iter()
        .map(|i| {
            if given < cap && is_national_focus(i) {
                let kind = NATIONAL_ELEMENTS[given % NATIONAL_ELEMENTS.len()].0;
                given += 1;
                Some(kind)
            } else {
                None
            }
        })
        .collect()
}

/// The kinds a plan hands out more than once (only when a set has more national icons than there are kinds).
pub fn national_repeats(plan: &[Option<&'static str>]) -> Vec<&'static str> {
    let mut seen = BTreeSet::new();
    let mut repeats = BTreeSet::new();
    for k in plan.iter().flatten() {
        if !seen.insert(*k) {
            repeats.insert(*k);
        }
    }
    repeats.into_iter().collect()
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
                && !previous.is_some_and(|p| COLOUR_FAMILIES.iter().any(|f| f.contains(&p) && f.contains(c)))
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

/// The brief of one icon in a set: the one frame the whole set shares (the vanilla national focus form), the colour
/// planned for it (it follows the subject, and green is never the main colour unless the plan says so), and the national
/// element only where the focus is directly national (`national` names its kind), none elsewhere.
pub fn icon_brief_coloured(icon: &SetIcon, set_size: usize, colour: &str, national: Option<&str>, nation: Option<&str>) -> String {
    let mut brief = icon_brief(icon, set_size);
    brief.push_str(&format!(
        " Colour: the enamel field behind the subject is a muted {colour}, as its subject calls for; the subject itself stays desaturated grey metal or sepia."
    ));
    if !GREEN_COLOURS.contains(&colour) {
        brief.push_str(" Green is not the colour of this icon's field (a national element may use its own colours).");
    }
    // a national element only where the country is known: "the country's flag" alone got another country's colours
    match (national.and_then(|k| NATIONAL_ELEMENTS.iter().find(|(name, _)| *name == k)), nation) {
        (Some((_, what)), Some(adj)) => brief.push_str(&format!(
            " This focus is directly national, so it carries exactly one national element, part of the scene and never a logo, badge or emblem stuck onto an object: {}. Use this kind of element only, not a flag or harp on top of it.",
            what.replace("the country's", &format!("the {adj}"))
        )),
        _ => brief.push_str(" No national symbols or emblems of any country and no national colours as trim."),
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
            key: None,
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
            key: None,
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
        let b = icon_brief_coloured(&icons[1], 6, plan[1], None, Some("Irish"));
        assert!(
            b.contains("a muted steel grey and khaki") && b.contains("Green is not the colour") && b.contains("No national symbols or emblems"),
            "{b}"
        );
        let flag = SetIcon { names: vec!["The Flag of the Republic".into()], ..icon_named("flag") };
        assert!(
            is_national_focus(&flag)
                && icon_brief_coloured(&flag, 6, "cream and gold with deep red", Some("flag"), Some("Irish")).contains("the Irish flag flying")
        );
        assert!(!is_national_focus(&icons[0]));
        let peat = SetIcon { names: vec!["Peat and Coal for Power".into()], ..icon_named("peat") };
        assert!(icon_brief_coloured(&peat, 6, "earth brown and tan", None, None).contains("not coal and not a mine"));
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
        let chosen: Vec<&str> = names.iter().zip(&plan).filter(|(_, y)| y.is_some()).map(|(n, _)| *n).collect();
        let kinds: Vec<&str> = plan.iter().flatten().copied().collect();
        assert_eq!(kinds, ["flag", "emblem", "knotwork"], "each national icon gets a different kind");
        assert!(national_repeats(&plan).is_empty());
        assert_eq!(chosen, ["Irish Neutrality", "Expand the Irish Army", "Gaelic Cultural Revival"]);
        // twelve armies: only a third carry the emblem
        let armies: Vec<SetIcon> = (0..12).map(|i| icon_named(&format!("Army {i}"))).collect();
        let refs: Vec<&SetIcon> = armies.iter().collect();
        let plan = national_plan(&refs);
        assert_eq!(plan.iter().flatten().count(), 4);
        assert!(national_repeats(&plan).is_empty(), "four kinds, none repeated");
        // hints keep the subjects apart: one handshake only
        let neutral = icon_brief_coloured(&icons[0], 12, "cream and soft grey", Some("flag"), Some("Irish"));
        let trade = icon_brief_coloured(&icons[11], 12, "gold and dark teal", None, Some("Irish"));
        // without a known country there is no national element at all
        let unknown = icon_brief_coloured(&icons[0], 12, "cream and soft grey", Some("sash"), None);
        assert!(unknown.contains("No national symbols") && !unknown.contains("flag colours"), "{unknown}");
        let named = |id: &str| SetIcon { focuses: vec![id.to_string()], ..icon_named(id) };
        let tagged: Vec<SetIcon> = ["IRE_army", "IRE_navy", "GER_x"].iter().map(|f| named(f)).collect();
        assert_eq!(tree_tag(&tagged).as_deref(), Some("IRE"));
        assert_eq!(tree_tag(&[named("army_reform"), named("navy")]), None);
        assert_eq!(adjective_keys("IRE")[1], "IRE_democratic_ADJ");
        assert!(neutral.contains("not a handshake") && trade.contains("not a handshake"));
    }

    #[test]
    fn key_focuses_come_from_the_trees_structure() {
        let tree = b"focus_tree = {
            id = t
            focus = { id = T_root }
            focus = { id = T_army prerequisite = { focus = T_root } }
            focus = { id = T_navy prerequisite = { focus = T_army } }
            focus = { id = T_air prerequisite = { focus = T_army } }
            focus = { id = T_left prerequisite = { focus = T_root } mutually_exclusive = { focus = T_right } }
            focus = { id = T_right prerequisite = { focus = T_root } mutually_exclusive = { focus = T_left } }
            focus = { id = T_end prerequisite = { focus = T_navy focus = T_air } }
            focus = { id = T_side prerequisite = { focus = T_left } }
        }"
        .to_vec();
        let nodes = focus_structure(tree);
        assert_eq!(nodes.len(), 8);
        assert_eq!(nodes[6].prerequisites, ["T_navy", "T_air"]);
        let w = focus_weight(&nodes);
        assert_eq!(w["T_root"].0, 5, "start, hub, and a third branch: {:?}", w["T_root"]);
        assert_eq!(w["T_army"].0, 2);
        assert_eq!(w["T_left"].0, 2);
        assert_eq!(w["T_end"].1, ["the end of a long branch"]);
        assert_eq!(w["T_side"].0, 0);
        let mut icons: Vec<SetIcon> = ["T_root", "T_army", "T_navy", "T_air", "T_left", "T_right", "T_end", "T_side"]
            .iter()
            .map(|f| SetIcon { focuses: vec![f.to_string()], ..icon_named(f) })
            .collect();
        mark_key_icons(&mut icons, &nodes);
        let keys: Vec<&str> = icons.iter().filter(|i| i.key.is_some()).map(|i| i.focuses[0].as_str()).collect();
        assert_eq!(keys, ["T_root", "T_army"], "a quarter of eight, the heaviest first");
        // a flat list has no key focuses
        let mut flat: Vec<SetIcon> = (0..8).map(|i| SetIcon { focuses: vec![format!("F_{i}")], ..icon_named("x") }).collect();
        mark_key_icons(&mut flat, &focus_structure(b"focus_tree = { focus = { id = F_0 } focus = { id = F_1 } }".to_vec()));
        assert!(flat.iter().all(|i| i.key.is_none()));
        // standout frames follow the subject and never repeat
        let a = SetIcon { key: Some("x".into()), names: vec!["Industrial Development".into()], ..icon_named("a") };
        let b = SetIcon { key: Some("x".into()), names: vec!["Factory Expansion".into()], ..icon_named("b") };
        let c = SetIcon { key: Some("x".into()), names: vec!["Irish Neutrality".into()], ..icon_named("c") };
        let plain = icon_named("d");
        let plan = standout_plan(&[&a, &plain, &b, &c]);
        assert_eq!(plan, [Some("cogwheel"), None, Some("sunburst"), Some("star_banners")]);
        assert!(
            standout_sentence("anchor", "the start of the tree").contains("great anchor")
                && standout_sentence("anchor", "x").contains("do not copy a frame")
        );
    }

    #[test]
    fn hands_go_to_one_icon_at_most() {
        let names = ["The People's Home", "Nordic Defence Union", "Social Welfare", "Nordic Cooperation", "Trade Pact"];
        let icons: Vec<SetIcon> = names.iter().map(|n| icon_named(n)).collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        assert_eq!(hands_plan(&refs), [false, true, false, false, false]);
        let none: Vec<SetIcon> = ["Navy", "Industry"].iter().map(|n| icon_named(n)).collect();
        assert!(hands_plan(&none.iter().collect::<Vec<_>>()).iter().all(|h| !h));
    }

    #[test]
    fn frames_vary_like_vanillas_and_neighbours_differ() {
        let names = [
            "Irish Neutrality",
            "Build the Irish Army",
            "A Coastal Navy",
            "The Irish Air Corps",
            "Industrial Development",
            "Peat Harvesting",
            "Modernise the Farms",
            "Gaelic Cultural Revival",
            "Scientific Research",
            "Civil Unrest",
            "Military Intelligence",
            "Atlantic Trade",
        ];
        let icons: Vec<SetIcon> = names.iter().map(|n| icon_named(n)).collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        let plan = frame_plan(&refs);
        assert_eq!(plan[1].0, "arms");
        assert_eq!(plan[3].0, "wings");
        for w in plan.windows(2) {
            assert_ne!(w[0].0, w[1].0, "{plan:?}");
        }
        let count = |k: &str| plan.iter().filter(|f| f.0 == k).count();
        assert!((3..=5).contains(&count("laurel")), "{plan:?}");
        assert!((1..=3).contains(&(count("shield") + count("diamond"))), "{plan:?}");
        assert_eq!(plan.iter().filter(|f| f.1).count(), 4);
        assert!(frame_sentence("oak", false).contains("oak-leaf wreath, with no ribbon banner"));
    }

    #[test]
    fn neighbours_never_share_a_colour_family() {
        let names = ["Navy", "Air Force", "Industry", "Peat Cutting"];
        let icons: Vec<SetIcon> = names.iter().map(|n| icon_named(n)).collect();
        let refs: Vec<&SetIcon> = icons.iter().collect();
        let plan = colour_plan(&refs);
        for w in plan.windows(2) {
            assert!(!COLOUR_FAMILIES.iter().any(|f| f.contains(&w[0]) && f.contains(&w[1])), "{plan:?}");
        }
        assert_eq!(plan[0], "navy and sea blue");
        assert_ne!(plan[1], "pale sky blue and silver", "air after navy takes another colour: {plan:?}");
    }
}
