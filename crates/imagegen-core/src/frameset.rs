//! Focus icon frames and pieces as data: the component library, the frame decision of a plan and the component plan
//! of a set.
//!
//! The main way to make a focus icon stays the model drawing the whole icon from the brief and the references
//! (`frame: "auto"` or `"model"`, the behaviour before this module). Component-built icons are there for variety and
//! consistency among generic icons:
//!
//! * The asset skill ships blank frames (`assets/visual_components/focus_backgrounds`) and transparent pieces
//!   (`assets/visual_components/focus_pieces`). The tool finds them under `.claude/skills` or `.agents/skills`, or a
//!   style pack names its own folders ([`PackFrames`]).
//! * More frames are made by the model: `frame: "blank"` asks for an empty frame, and once a person has looked at it,
//!   `frame add` puts it into the project's library (`.hoi4-imagegen/frames`, [`FrameManifest`]).
//! * A frame key places a subject on that frame's plate ([`crate::recipes::icon_frame`]): a piece imported with
//!   `hoi4.image_process`, or a subject the model drew alone.
//! * `hoi4.image_set` builds a share of a tree's icons from components ([`plan_components`]): only icons whose focus
//!   names a subject one of the pieces shows, spread through the set, no two frames or pieces alike in a row. The rest
//!   are made from scratch.
//!
//! Everything here is a pure function of its inputs.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::family::Family;
use crate::hashing::sha256_hex;

/// Folder of the project's own frames (frames the model drew and a person added).
pub const PROJECT_DIR: &str = ".hoi4-imagegen/frames";
/// File name of a frame folder's manifest.
pub const MANIFEST_FILE: &str = "frames.toml";
/// Folder of the skill's blank frames, inside `assets/visual_components`.
pub const FRAMES_SUBDIR: &str = "focus_backgrounds";
/// Folder of the skill's pieces, inside `assets/visual_components`.
pub const PIECES_SUBDIR: &str = "focus_pieces";
/// The model draws the whole icon, frame included (the main way, and the default).
pub const MODEL: &str = "model";
/// The tool picks: the model draws the icon. Kept apart from `model` only so requests can say it.
pub const AUTO: &str = "auto";
/// The model draws an empty frame, for the library.
pub const BLANK: &str = "blank";
/// Share of a set built from components when nothing says otherwise.
pub const DEFAULT_COMPONENT_SHARE: f64 = 0.25;
/// Version of the frame recipe (bump when an output could change).
pub const FRAME_VERSION: u32 = 1;
/// Largest manifest read.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// The shape of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    /// A round plate, medallion, ring or wreath.
    Round,
    /// A diamond.
    Diamond,
    /// A square or rectangular plaque.
    Square,
    /// A heraldic shield.
    Shield,
    /// Anything else.
    Other,
}

impl Form {
    /// Name for output.
    pub fn as_str(self) -> &'static str {
        match self {
            Form::Round => "round",
            Form::Diamond => "diamond",
            Form::Square => "square",
            Form::Shield => "shield",
            Form::Other => "other",
        }
    }
}

/// One frame of a folder's `frames.toml`. Every field but `file` is optional: the box and the form are measured
/// when absent ([`crate::framebox`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameEntry {
    /// PNG file name in the folder.
    pub file: String,
    /// Key (default: the file's stem without `focus_background_`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Form>,
    /// Placement box `[x0, y0, x1, y1]` in the file's pixels (x1 and y1 exclusive).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "box")]
    pub place: Option<[u32; 4]>,
    /// Relative weight when a set picks frames (default: vanilla's mix by form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    /// False for a frame that waits for a person's look; it is listed but never used.
    #[serde(default = "yes")]
    pub approved: bool,
    /// SHA-256 of the file when it was added (a changed file is refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Where the frame came from (for example the candidate it was made from).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn yes() -> bool {
    true
}

/// A frame folder's manifest (`frames.toml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameManifest {
    /// Schema version.
    #[serde(default = "one")]
    pub schema: u32,
    /// The frames.
    #[serde(default, rename = "frame")]
    pub frames: Vec<FrameEntry>,
}

fn one() -> u32 {
    1
}

fn plain_file_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) && !name.contains("..")
}

impl FrameManifest {
    /// Parse and check.
    pub fn parse(text: &str) -> Result<FrameManifest> {
        let m: FrameManifest = toml::from_str(text).map_err(|e| Error::Invalid(format!("frames.toml: {e}")))?;
        m.check()?;
        Ok(m)
    }

    /// As TOML.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Invalid(format!("frames.toml: {e}")))
    }

    /// Check file names, keys, boxes and weights.
    pub fn check(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::Invalid(format!("frames.toml: schema {} is not supported (this tool reads schema 1)", self.schema)));
        }
        let mut keys: Vec<String> = Vec::new();
        for f in &self.frames {
            if !plain_file_name(&f.file) || !f.file.to_ascii_lowercase().ends_with(".png") {
                return Err(Error::Invalid(format!("frames.toml: {:?} is not a plain .png file name", f.file)));
            }
            let key = f.key.clone().unwrap_or_else(|| key_of(&f.file));
            check_key(&key)?;
            if keys.contains(&key) {
                return Err(Error::Invalid(format!("frames.toml names the frame {key} twice")));
            }
            keys.push(key);
            if let Some([x0, y0, x1, y1]) = f.place {
                if x0 >= x1 || y0 >= y1 || x1 > 4096 || y1 > 4096 {
                    return Err(Error::Invalid(format!("frames.toml: the box of {} is empty", f.file)));
                }
            }
            if let Some(w) = f.weight {
                if !(w.is_finite() && (0.0..=100.0).contains(&w)) {
                    return Err(Error::Invalid(format!("frames.toml: the weight of {} must be 0 to 100", f.file)));
                }
            }
        }
        Ok(())
    }
}

/// A frame key: snake_case and not a reserved word.
pub fn check_key(key: &str) -> Result<()> {
    crate::naming::validate_snake("frame key", key)?;
    if [MODEL, AUTO, BLANK, "none"].contains(&key) {
        return Err(Error::Invalid(format!("frame key {key:?} is reserved; pick another")));
    }
    Ok(())
}

/// The key of a frame or piece file: its stem, lower case, without `focus_background_` or `focus_piece_`.
pub fn key_of(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s).to_ascii_lowercase();
    let stem = stem.strip_prefix("focus_background_").or_else(|| stem.strip_prefix("focus_piece_")).unwrap_or(&stem).to_string();
    let mut out: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.starts_with(|c: char| c.is_ascii_alphabetic()) {
        out
    } else {
        format!("f_{out}")
    }
}

/// A style pack's component folders for focus icons.
///
/// ```toml
/// [families.focus.frames]
/// dir = "frames"      # blank frames (PNGs, optional frames.toml) inside the pack's folder
/// pieces = "pieces"   # transparent pieces inside the pack's folder
/// share = 0.2         # share of a set built from components (0 turns it off)
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackFrames {
    /// Frames folder, relative to the pack's folder. Without it the pack's sets are made from scratch only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Pieces folder, relative to the pack's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pieces: Option<String>,
    /// Share of a set built from components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<f64>,
}

impl PackFrames {
    /// Check the folders and the share.
    pub fn check(&self) -> Result<()> {
        for d in [&self.dir, &self.pieces].into_iter().flatten() {
            crate::fsio::check_relative(d)?;
        }
        if let Some(s) = self.share {
            if !(0.0..=1.0).contains(&s) {
                return Err(Error::Invalid("frames.share must be 0 to 1".into()));
            }
        }
        Ok(())
    }
}

/// A frame of the library, resolved.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Frame {
    /// Key.
    pub key: String,
    /// Shape.
    pub form: Form,
    /// Project-relative path of the PNG.
    pub path: String,
    /// SHA-256 of the file.
    pub sha256: String,
    /// Size of the PNG.
    pub size: [u32; 2],
    /// Placement box in the file's pixels.
    #[serde(rename = "box")]
    pub place: [u32; 4],
    /// How the box was found: `known` (a frame the skill ships), `manifest`, `hole`, `plate` or `fallback`.
    pub box_from: String,
    /// Relative weight.
    pub weight: f64,
    /// `skill`, `project` or `pack:<name>`.
    pub origin: String,
}

/// A piece of the library.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Piece {
    /// Key (`tank`, `cog_wheel`...).
    pub key: String,
    /// Project-relative path of the PNG.
    pub path: String,
    /// SHA-256 of the file.
    pub sha256: String,
    /// Words in a focus name that this piece shows.
    pub words: Vec<String>,
}

/// Extra words for the pieces the skill ships, beyond the words of their names.
const PIECE_WORDS: &[(&str, &[&str])] = &[
    ("ballot", &["election", "elections", "vote", "votes", "democracy", "democratic", "referendum", "parliament"]),
    ("barbed_wire", &["fortify", "fortification", "fortifications", "defence", "defense", "camp"]),
    ("battleship", &["navy", "naval", "fleet", "ship", "ships", "battleships", "dreadnought"]),
    ("binoculars", &["intelligence", "reconnaissance", "scout", "observe", "watch"]),
    ("blueprints", &["plan", "plans", "design", "designs", "engineering", "project"]),
    ("bomb_nuclear", &["atomic", "bomb", "bombs"]),
    ("book_open", &["education", "school", "schools", "university", "literacy", "constitution", "history"]),
    ("brain", &["science", "intellect", "psychology", "mind"]),
    ("bread", &["food", "agriculture", "farm", "farms", "harvest", "grain", "hunger", "welfare"]),
    ("cannon", &["artillery", "guns", "gun", "siege"]),
    ("cog_wheel", &["industry", "industrial", "factory", "factories", "production", "machine", "machines"]),
    ("dove", &["peace", "neutrality", "neutral", "pacifism", "pacifist", "reconciliation"]),
    ("electricity", &["electric", "electrification", "power", "grid", "hydroelectric"]),
    ("fist", &["revolution", "revolt", "strike", "uprising", "resistance"]),
    ("hands_shaking", &["alliance", "ally", "treaty", "pact", "agreement", "diplomacy", "cooperation", "friendship"]),
    ("laborer", &["labour", "labor", "workers", "worker", "union", "unions", "employment"]),
    ("lightning_bolt", &["blitz", "blitzkrieg", "lightning", "shock"]),
    ("map_with_arrow", &["expansion", "claim", "claims", "invasion", "campaign", "annex", "conquest", "border"]),
    ("missile", &["missiles", "rocketry", "ballistic"]),
    ("nuclear_atom", &["atom", "atomic", "nuclear", "physics", "research"]),
    ("oil_droplet", &["oil", "fuel", "petroleum", "refinery", "refineries"]),
    ("paper_torn", &["denounce", "repudiate", "versailles", "renounce"]),
    ("railroad", &["railway", "railways", "rail", "rails", "infrastructure"]),
    ("rocket", &["rockets", "space", "jet"]),
    ("skull", &["death", "terror", "purge", "purges", "massacre"]),
    ("soldier_captured", &["prisoner", "prisoners", "surrender", "capture"]),
    ("soldiers_charging", &["army", "infantry", "offensive", "attack", "assault", "charge", "mobilisation", "mobilization", "conscription"]),
    ("sword", &["war", "military", "militarism", "warrior", "honour", "honor"]),
    ("tank", &["tanks", "armour", "armor", "armoured", "armored", "mechanised", "mechanized", "panzer"]),
    ("torch", &["enlightenment", "liberty", "freedom", "flame"]),
    ("train", &["trains", "locomotive", "transport", "logistics"]),
    ("vial", &["chemistry", "chemical", "medicine", "medical", "health", "laboratory"]),
    ("wrench", &["repair", "maintenance", "engineers", "workshop", "tools"]),
];

/// The words a piece key shows: its own words and, for a piece the skill ships, its extra words.
pub fn piece_words(key: &str) -> Vec<String> {
    let mut w: Vec<String> = key.split('_').filter(|t| t.len() > 2).map(str::to_string).collect();
    if let Some((_, extra)) = PIECE_WORDS.iter().find(|(k, _)| *k == key) {
        w.extend(extra.iter().map(|s| s.to_string()));
    }
    w.sort();
    w.dedup();
    w
}

/// The words of a focus text, lower case.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// The pieces that show what `text` names, best first (most matching words, then the key).
pub fn matching_pieces<'a>(text: &str, pieces: &'a [Piece]) -> Vec<&'a Piece> {
    let ws = words(text);
    let mut scored: Vec<(usize, &Piece)> =
        pieces.iter().map(|p| (p.words.iter().filter(|w| ws.contains(w)).count(), p)).filter(|(n, _)| *n > 0).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));
    scored.into_iter().map(|(_, p)| p).collect()
}

/// A 64-bit seed from a text.
pub fn seed_of(text: &str) -> u64 {
    let h = sha256_hex(text.as_bytes());
    u64::from_str_radix(&h[..16], 16).unwrap_or(1)
}

/// What a set builds from components for one icon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentPick {
    /// The frame key.
    pub frame: String,
    /// The piece key.
    pub piece: String,
}

/// Which icons of a set are built from components. `texts[i]` is icon `i`'s focus names; `share` caps the number
/// built (rounded, at least one when any icon qualifies and the set has four or more). Only icons whose text a piece
/// shows qualify; no two built icons are neighbours where the count allows; each built icon takes the best unused
/// matching piece, and the frames are dealt out by weight so that consecutive built icons get different frames.
pub fn plan_components(texts: &[String], frames: &[Frame], pieces: &[Piece], share: f64, seed: u64) -> Vec<Option<ComponentPick>> {
    let n = texts.len();
    let mut out: Vec<Option<ComponentPick>> = vec![None; n];
    let usable: Vec<&Frame> = frames.iter().filter(|f| f.weight > 0.0).collect();
    if usable.is_empty() || pieces.is_empty() || share <= 0.0 {
        return out;
    }
    let candidates: Vec<(usize, Vec<&Piece>)> =
        texts.iter().enumerate().map(|(i, t)| (i, matching_pieces(t, pieces))).filter(|(_, m)| !m.is_empty()).collect();
    let mut cap = (n as f64 * share).round() as usize;
    if cap == 0 && n >= 4 && !candidates.is_empty() {
        cap = 1;
    }
    // spread: a first pass keeps a gap of one, a second fills up to the cap if the gaps left too few
    let mut chosen: Vec<usize> = Vec::new();
    for strict in [true, false] {
        for (i, _) in &candidates {
            if chosen.len() >= cap {
                break;
            }
            if chosen.contains(i) || (strict && chosen.iter().any(|c| c.abs_diff(*i) <= 1)) {
                continue;
            }
            chosen.push(*i);
        }
    }
    chosen.sort_unstable();
    // frames: a weighted deck in an order fixed by the seed, dealt in turn
    let mut deck: Vec<&Frame> = Vec::new();
    let max_w = usable.iter().map(|f| f.weight).fold(0.0, f64::max);
    for f in &usable {
        let copies = ((f.weight / max_w) * 3.0).round().max(1.0) as usize;
        deck.extend(std::iter::repeat_n(*f, copies));
    }
    deck.sort_by_key(|f| (seed_of(&format!("{seed}/{}", f.key)), f.key.clone()));
    let mut used_pieces: Vec<String> = Vec::new();
    let mut last_frame: Option<String> = None;
    let mut at = 0usize;
    for i in chosen {
        let matches = &candidates.iter().find(|(j, _)| *j == i).expect("chosen from the candidates").1;
        let piece = matches.iter().find(|p| !used_pieces.contains(&p.key)).unwrap_or(&matches[0]);
        used_pieces.push(piece.key.clone());
        let mut frame = deck[at % deck.len()];
        for step in 0..deck.len() {
            let f = deck[(at + step) % deck.len()];
            if Some(&f.key) != last_frame.as_ref() || usable.len() == 1 {
                frame = f;
                at += step;
                break;
            }
        }
        at += 1;
        last_frame = Some(frame.key.clone());
        out[i] = Some(ComponentPick { frame: frame.key.clone(), piece: piece.key.clone() });
    }
    out
}

/// Plates and pieces must differ at least this much in mean luminance (0 to 255), or the piece is lost on its plate.
pub const MIN_CONTRAST: f64 = 70.0;
/// The luminance a clear plate (a ring) counts as: the focus tree's dark background shows through it.
pub const CLEAR_PLATE_LUMA: f64 = 50.0;

/// Mean luminance of the solid pixels of `img` inside `rect` (`[x0, y0, x1, y1]`), or `None` when none are solid.
pub fn mean_luma(img: &crate::pixels::Rgba, rect: [u32; 4]) -> Option<f64> {
    let (mut sum, mut n) = (0f64, 0u32);
    for y in rect[1]..rect[3].min(img.height) {
        for x in rect[0]..rect[2].min(img.width) {
            let p = img.px(x, y);
            if p[3] >= 128 {
                sum += 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]);
                n += 1;
            }
        }
    }
    (n > 0).then(|| sum / f64::from(n))
}

/// Swap the frame of a component pick whose piece would be lost on its plate (luminance closer than [`MIN_CONTRAST`])
/// for the frame with the most contrast that differs from the neighbouring picks' frames, when that one clears the
/// floor or beats the current plate by ten levels or more. `plates` is each frame's
/// plate luminance, `pieces` each piece's.
pub fn fix_contrast(plan: &mut [Option<ComponentPick>], plates: &BTreeMap<String, f64>, pieces: &BTreeMap<String, f64>) {
    let idx: Vec<usize> = plan.iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| i).collect();
    for (k, &i) in idx.iter().enumerate() {
        let pick = plan[i].clone().expect("a pick");
        let (Some(piece), Some(plate)) = (pieces.get(&pick.piece), plates.get(&pick.frame)) else { continue };
        if (piece - plate).abs() >= MIN_CONTRAST {
            continue;
        }
        let near: Vec<String> = [k.checked_sub(1), Some(k + 1)]
            .into_iter()
            .flatten()
            .filter_map(|j| idx.get(j))
            .filter_map(|&j| plan[j].as_ref().map(|p| p.frame.clone()))
            .collect();
        // the frame with the most contrast; taken when it clears the floor, or at least beats the current one clearly
        let now = (piece - plate).abs();
        let best = plates
            .iter()
            .filter(|(f, _)| !near.contains(f))
            .map(|(f, l)| (f, (piece - l).abs()))
            .filter(|(_, d)| *d >= MIN_CONTRAST || *d >= now + 10.0)
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(a.0)));
        if let Some((f, _)) = best {
            plan[i] = Some(ComponentPick { frame: f.clone(), piece: pick.piece });
        }
    }
}

/// What a plan or an import decided about the frame of one focus icon. Recorded in the plan and in the candidate,
/// so processing places the subject on the same frame. Absent when the model draws the whole icon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FramePlan {
    /// A frame key of the library, or `blank` (the model draws an empty frame).
    pub key: String,
    /// `skill`, `project` or `pack:<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Project-relative path of the frame's PNG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// SHA-256 of the frame's PNG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Placement box in the frame's pixels.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "box")]
    pub place: Option<[u32; 4]>,
    /// The piece the subject is, when the icon is built from components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece: Option<String>,
    /// Why, in a sentence.
    pub note: String,
}

impl FramePlan {
    /// True when the tool places the subject on a frame of the library.
    pub fn on_frame(&self) -> bool {
        self.key != BLANK && self.path.is_some()
    }
}

/// The prompt rule that asks for the subject only, in place of the family's form sentence, when a subject the model
/// draws goes on a frame of the library. It does not start with "The Hearts of Iron IV ... form", so a style anchor's
/// form rule never replaces it.
pub const SUBJECT_ONLY_RULE: &str = "Draw the SUBJECT ONLY of a national focus icon, painted in aged, painterly metal and enamel like Hearts of Iron IV interface art, with a clear silhouette on a real transparent background. The tool places it on its own frame afterwards, so draw no frame, no plaque, no medallion, no wreath, no ribbon, no border and no backdrop; take only the painting style from any reference image.";

/// The prompt rule for `frame: "blank"`: an empty frame for the library.
pub const BLANK_FRAME_RULE: &str = "Draw an EMPTY frame for a Hearts of Iron IV national focus icon, in aged painterly metal and enamel like vanilla focus art: a round plate, medallion, ring or heraldic shield with its rim, wreath, ribbons or crown as the brief asks, centred, on a real transparent background. The plate in the middle is plain and empty, a smooth flat colour or metal with no symbol, figure, text or emblem on it: a subject is placed there later.";

/// The size rule that goes with [`SUBJECT_ONLY_RULE`] and [`BLANK_FRAME_RULE`] in place of the family's "may overlap
/// its frame" rule.
pub const SUBJECT_SIZE_RULE: &str = "It is large and fills most of the picture, one strong central shape, never small in empty space.";

/// Families whose icons the tool can build from components.
pub fn framed_family(f: Family) -> bool {
    f == Family::Focus
}

/// The weights of the frames by form when a set picks among them: vanilla's mix of round plates (seven in ten),
/// diamond and square plaques (one in ten) and shields (one in fifteen), split evenly within a form.
pub fn form_weights(forms: &[Form]) -> Vec<f64> {
    let share = |f: Form| match f {
        Form::Round | Form::Other => 0.70,
        Form::Diamond | Form::Square => 0.10,
        Form::Shield => 1.0 / 15.0,
    };
    let mut count: BTreeMap<Form, usize> = BTreeMap::new();
    for f in forms {
        *count.entry(*f).or_default() += 1;
    }
    forms.iter().map(|f| share(*f) / count[f] as f64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(key: &str, form: Form, weight: f64) -> Frame {
        Frame {
            key: key.into(),
            form,
            path: format!("x/{key}.png"),
            sha256: "0".repeat(64),
            size: [100, 88],
            place: [20, 15, 80, 73],
            box_from: "known".into(),
            weight,
            origin: "skill".into(),
        }
    }

    fn piece(key: &str) -> Piece {
        Piece { key: key.into(), path: format!("x/{key}.png"), sha256: "0".repeat(64), words: piece_words(key) }
    }

    #[test]
    fn keys_come_from_file_names() {
        assert_eq!(key_of("focus_background_circle_ribbon_2.png"), "circle_ribbon_2");
        assert_eq!(key_of("focus_piece_hands_shaking.png"), "hands_shaking");
        assert_eq!(key_of("My Frame (2).PNG"), "my_frame_2");
        assert_eq!(key_of("2nd.png"), "f_2nd");
        assert!(check_key("blank").is_err());
    }

    #[test]
    fn pieces_match_the_words_a_focus_names() {
        let ps = vec![piece("tank"), piece("cog_wheel"), piece("hands_shaking"), piece("dove")];
        assert_eq!(matching_pieces("Expand the Armoured Divisions", &ps)[0].key, "tank");
        assert_eq!(matching_pieces("Industrial Expansion", &ps)[0].key, "cog_wheel");
        assert_eq!(matching_pieces("A Treaty of Friendship", &ps)[0].key, "hands_shaking");
        assert!(matching_pieces("The Easter Rising", &ps).is_empty());
    }

    #[test]
    fn a_share_of_a_set_is_built_spread_and_varied() {
        let texts: Vec<String> = [
            "Industrial Effort",
            "Expand the Factories",
            "Tank Doctrine",
            "The Easter Rising",
            "Peace in Europe",
            "Armoured Push",
            "Railway Network",
            "Civil Service",
            "Election Reform",
            "Oil for the Fleet",
            "Gaelic Revival",
            "Naval Yards",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let frames = vec![frame("ribbon", Form::Round, 0.35), frame("ring", Form::Round, 0.35), frame("shield", Form::Shield, 0.07)];
        let pieces: Vec<Piece> = ["tank", "cog_wheel", "dove", "railroad", "ballot", "oil_droplet", "battleship"].iter().map(|k| piece(k)).collect();
        let plan = plan_components(&texts, &frames, &pieces, 0.25, 9);
        let built: Vec<usize> = plan.iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| i).collect();
        assert_eq!(built.len(), 3, "{plan:?}");
        for w in built.windows(2) {
            assert!(w[1] - w[0] >= 2, "neighbours: {built:?}");
        }
        let picks: Vec<&ComponentPick> = plan.iter().flatten().collect();
        for w in picks.windows(2) {
            assert_ne!(w[0].frame, w[1].frame, "{picks:?}");
            assert_ne!(w[0].piece, w[1].piece, "{picks:?}");
        }
        // the icons no piece shows are never built
        assert!(plan[3].is_none() && plan[10].is_none());
        assert_eq!(plan, plan_components(&texts, &frames, &pieces, 0.25, 9));
        // share 0, no frames or no pieces: everything from scratch
        assert!(plan_components(&texts, &frames, &pieces, 0.0, 9).iter().all(Option::is_none));
        assert!(plan_components(&texts, &[], &pieces, 0.25, 9).iter().all(Option::is_none));
        assert!(plan_components(&texts, &frames, &[], 0.25, 9).iter().all(Option::is_none));
    }

    #[test]
    fn a_pale_piece_leaves_a_pale_plate() {
        let mut plan = vec![
            Some(ComponentPick { frame: "white_shield".into(), piece: "dove".into() }),
            None,
            Some(ComponentPick { frame: "red".into(), piece: "tank".into() }),
        ];
        let plates: BTreeMap<String, f64> =
            [("white_shield".to_string(), 200.0), ("red".to_string(), 70.0), ("ring".to_string(), CLEAR_PLATE_LUMA)].into();
        let pieces: BTreeMap<String, f64> = [("dove".to_string(), 190.0), ("tank".to_string(), 40.0)].into();
        fix_contrast(&mut plan, &plates, &pieces);
        // the dove moves to the frame with most contrast that is not its neighbour's (red is the next pick's)
        assert_eq!(plan[0].as_ref().unwrap().frame, "ring");
        // the dark tank on the red plate is 30 apart: it moves to the white shield (160 apart)
        assert_eq!(plan[2].as_ref().unwrap().frame, "white_shield");
        let img = crate::pixels::Rgba::filled(4, 4, [255, 255, 255, 255]);
        assert!((mean_luma(&img, [0, 0, 4, 4]).unwrap() - 255.0).abs() < 1e-6);
        assert!(mean_luma(&crate::pixels::Rgba::new(4, 4), [0, 0, 4, 4]).is_none());
    }

    #[test]
    fn manifest_round_trips_and_refuses_bad_entries() {
        let m = FrameManifest {
            schema: 1,
            frames: vec![FrameEntry {
                file: "laurel_plate.png".into(),
                key: None,
                form: Some(Form::Round),
                place: Some([20, 14, 80, 74]),
                weight: Some(1.0),
                approved: false,
                sha256: Some("a".repeat(64)),
                note: "from iron_pact.frame-v1".into(),
            }],
        };
        let text = m.to_toml().unwrap();
        assert_eq!(FrameManifest::parse(&text).unwrap(), m);
        let mut bad = m.clone();
        bad.frames[0].file = "../x.png".into();
        assert!(bad.check().is_err());
        let mut bad = m.clone();
        bad.frames[0].place = Some([20, 14, 10, 74]);
        assert!(bad.check().is_err());
        let mut dup = m.clone();
        dup.frames.push(m.frames[0].clone());
        assert!(dup.check().is_err());
    }

    #[test]
    fn form_weights_follow_vanillas_mix() {
        let w = form_weights(&[Form::Round, Form::Round, Form::Shield]);
        assert!((w[0] - 0.35).abs() < 1e-9 && (w[2] - 1.0 / 15.0).abs() < 1e-9);
        assert!(PackFrames { dir: Some("../x".into()), ..Default::default() }.check().is_err());
        assert!(PackFrames { share: Some(1.5), ..Default::default() }.check().is_err());
    }
}
