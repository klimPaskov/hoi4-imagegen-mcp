//! The focus icon component library: blank frames and pieces, the frame decision of a plan, placing a subject on a
//! frame, and `frame list` / `frame add`.
//!
//! See `imagegen_core::frameset`. The model drawing the whole icon stays the main way; the library is for variety and
//! consistency among generic icons. Frames and pieces are read where the asset skill keeps them
//! (`.claude/skills/*/assets/visual_components/` or `.agents/skills/*/...`, or the `[templates] dir` folder), from a
//! style pack's own folders, and from the project's `.hoi4-imagegen/frames`, where `frame add` puts frames the model
//! drew with `frame: "blank"`. Nothing is copied out of the project, and the library's own files are never changed.

use imagegen_core::error::RetryClass;
use imagegen_core::family::Family;
use imagegen_core::framebox;
use imagegen_core::frameset::{
    self, form_weights, framed_family, key_of, piece_words, Form, Frame, FrameEntry, FrameManifest, FramePlan, Piece, AUTO, BLANK, FRAMES_SUBDIR,
    MANIFEST_FILE, MODEL, PIECES_SUBDIR, PROJECT_DIR,
};
use imagegen_core::fsio::display_path;
use imagegen_core::hashing::sha256_hex;
use imagegen_core::pack::{StylePack, PACK_DIR};
use imagegen_core::pixels::{decode, encode_png, DecodeLimits, Rgba};
use imagegen_core::recipes::icon_frame;
use serde_json::{json, Value};

use super::{EResult, Engine, EngineError};

/// Largest frame or piece file read.
const MAX_COMPONENT_BYTES: usize = 8 * 1024 * 1024;
/// Most files read from one folder.
const MAX_FILES: usize = 400;

/// The component library in force for a plan or a set.
#[derive(Debug, Clone, Default)]
pub struct Library {
    /// Approved frames.
    pub frames: Vec<Frame>,
    /// Frames added but not yet approved (listed, never used).
    pub pending: Vec<Frame>,
    /// Pieces.
    pub pieces: Vec<Piece>,
    /// The folders read.
    pub sources: Vec<String>,
    /// Share of a set built from components.
    pub share: f64,
}

impl Library {
    /// The frame with `key`.
    pub fn frame(&self, key: &str) -> Option<&Frame> {
        self.frames.iter().find(|f| f.key == key)
    }

    /// The piece with `key`.
    pub fn piece(&self, key: &str) -> Option<&Piece> {
        self.pieces.iter().find(|p| p.key == key)
    }
}

/// Ask for the subject only (a frame of the library) or an empty frame (`blank`): the family's form sentence and its
/// "may overlap its frame" rule give way, and the frame words join the negatives for a subject. Matches by content, so
/// edits to the rest of the family's rules stand.
pub fn apply_prompt(profile: &mut imagegen_core::profile::Profile, key: &str) {
    let form = if key == BLANK { frameset::BLANK_FRAME_RULE } else { frameset::SUBJECT_ONLY_RULE };
    let rules = &mut profile.imagegen.prompt.rules;
    let mut done = false;
    for r in rules.iter_mut() {
        if r.contains("do not share one frame") {
            *r = form.to_string();
            done = true;
        } else if r.contains("may overlap its frame") || (key == BLANK && r.contains("strong central symbol")) {
            *r = frameset::SUBJECT_SIZE_RULE.to_string();
        }
    }
    if !done {
        rules.insert(0, form.to_string());
    }
    rules.dedup();
    if key != BLANK {
        let neg = &mut profile.imagegen.prompt.negative;
        for n in ["plaque", "medallion", "laurel wreath", "frame", "border", "backdrop"] {
            if !neg.iter().any(|x| x == n) {
                neg.push(n.to_string());
            }
        }
    }
}

fn is_png(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".png") && !l.starts_with("contact_sheet") && !l.starts_with("contact-sheet")
}

impl Engine {
    /// The asset skills' `assets/visual_components` folders, in search order: the `[templates] dir` folder's (or the
    /// folder itself when it holds the frames), then every skill under `.claude/skills` and `.agents/skills`.
    pub(crate) fn component_bases(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(dir) = &self.config.templates.dir {
            let d = dir.trim_end_matches(['/', '\\']).to_string();
            out.push(format!("{d}/visual_components"));
            out.push(d);
        }
        for root in [".claude/skills", ".agents/skills"] {
            let mut skills = self.fs.list_names(root).unwrap_or_default();
            skills.sort();
            out.extend(skills.into_iter().map(|s| format!("{root}/{s}/assets/visual_components")));
        }
        out.retain(|b| self.fs.exists(&format!("{b}/{FRAMES_SUBDIR}")) || self.fs.exists(&format!("{b}/{PIECES_SUBDIR}")));
        out
    }

    fn read_component(&self, rel: &str) -> EResult<(Vec<u8>, Rgba)> {
        let bytes = self.fs.read(rel, MAX_COMPONENT_BYTES)?;
        let (img, _) = decode(&bytes, Some(rel), DecodeLimits::default())?;
        Ok((bytes, img))
    }

    /// The frames of one folder: its PNGs, with `frames.toml` entries where it has them. In the project's own folder
    /// only the manifest's files count (a frame is added with `frame add`).
    fn frames_in(&self, dir: &str, origin: &str, manifest_only: bool) -> EResult<(Vec<Frame>, Vec<Frame>)> {
        let manifest = match self.fs.read(&format!("{dir}/{MANIFEST_FILE}"), frameset::MAX_MANIFEST_BYTES) {
            Ok(b) => FrameManifest::parse(&String::from_utf8_lossy(&b))?,
            Err(imagegen_core::Error::NotFound(_)) => FrameManifest::default(),
            Err(e) => return Err(e.into()),
        };
        let mut names: Vec<String> = if manifest_only {
            manifest.frames.iter().map(|e| e.file.clone()).collect()
        } else {
            let mut n: Vec<String> = self.fs.list_names(dir).unwrap_or_default().into_iter().filter(|n| is_png(n)).collect();
            n.sort();
            n.truncate(MAX_FILES);
            n
        };
        names.dedup();
        let mut rows: Vec<(Frame, bool, Option<f64>)> = Vec::new();
        for name in names {
            let rel = format!("{dir}/{name}");
            let entry: Option<&FrameEntry> = manifest.frames.iter().find(|e| e.file == name);
            let Ok((bytes, img)) = self.read_component(&rel) else { continue };
            let sha = sha256_hex(&bytes).to_ascii_lowercase();
            if let Some(want) = entry.and_then(|e| e.sha256.as_deref()) {
                if !want.eq_ignore_ascii_case(&sha) {
                    return Err(EngineError::refused(
                        "frame_changed",
                        format!("{rel} changed after it was added to {dir}/{MANIFEST_FILE}; add it again with frame add"),
                        RetryClass::RefreshPreconditions,
                    ));
                }
            }
            let known = framebox::known(&sha);
            let (place, box_from) = match (entry.and_then(|e| e.place), known) {
                (Some(b), _) => (b, "manifest".to_string()),
                (None, Some((_, b))) => (b, "known".to_string()),
                (None, None) => match framebox::measure(&img) {
                    Some((b, m)) => (b, m.as_str().to_string()),
                    None => continue,
                },
            };
            let form = entry.and_then(|e| e.form).or(known.map(|k| k.0)).unwrap_or_else(|| framebox::guess_form(&img));
            let key = entry.and_then(|e| e.key.clone()).unwrap_or_else(|| key_of(&name));
            let frame =
                Frame { key, form, path: rel, sha256: sha, size: [img.width, img.height], place, box_from, weight: 0.0, origin: origin.to_string() };
            rows.push((frame, entry.is_none_or(|e| e.approved), entry.and_then(|e| e.weight)));
        }
        let forms: Vec<Form> = rows.iter().map(|r| r.0.form).collect();
        let by_form = form_weights(&forms);
        let (mut approved, mut pending) = (Vec::new(), Vec::new());
        for ((mut f, ok, w), dw) in rows.into_iter().zip(by_form) {
            let dw = if framebox::not_picked(&f.sha256) { 0.0 } else { dw };
            f.weight = (w.unwrap_or(dw) * 10000.0).round() / 10000.0;
            if ok {
                approved.push(f);
            } else {
                pending.push(f);
            }
        }
        Ok((approved, pending))
    }

    fn pieces_in(&self, dir: &str) -> Vec<Piece> {
        let mut names: Vec<String> = self.fs.list_names(dir).unwrap_or_default().into_iter().filter(|n| is_png(n)).collect();
        names.sort();
        names.truncate(MAX_FILES);
        names
            .into_iter()
            .filter_map(|n| {
                let rel = format!("{dir}/{n}");
                let bytes = self.fs.read(&rel, MAX_COMPONENT_BYTES).ok()?;
                let key = key_of(&n);
                Some(Piece { words: piece_words(&key), key, path: rel, sha256: sha256_hex(&bytes).to_ascii_lowercase() })
            })
            .collect()
    }

    /// The library for a family under a pack (or none). A pack with a `frames` entry uses its own folders; a pack
    /// without one builds nothing from components (its house style is not vanilla's frames); with no pack the skill's
    /// components apply. The project's own approved frames join wherever frames apply.
    pub(crate) fn library(&self, pack: Option<&StylePack>, family: Family) -> EResult<Library> {
        let mut lib = Library { share: frameset::DEFAULT_COMPONENT_SHARE, ..Default::default() };
        if !framed_family(family) {
            return Ok(lib);
        }
        let mut frame_dirs: Vec<(String, String)> = Vec::new();
        let mut piece_dirs: Vec<String> = Vec::new();
        match pack {
            Some(p) => {
                let Some(cfg) = p.families.get(&family).and_then(|e| e.frames.as_ref()) else {
                    lib.share = 0.0;
                    return Ok(lib);
                };
                cfg.check()?;
                lib.share = cfg.share.unwrap_or(frameset::DEFAULT_COMPONENT_SHARE);
                let base = format!("{PACK_DIR}/{}", p.name);
                if let Some(d) = &cfg.dir {
                    frame_dirs.push((format!("{base}/{d}"), format!("pack:{}", p.name)));
                }
                match &cfg.pieces {
                    Some(d) => piece_dirs.push(format!("{base}/{d}")),
                    None => piece_dirs.extend(self.component_bases().into_iter().map(|b| format!("{b}/{PIECES_SUBDIR}"))),
                }
            }
            None => {
                for b in self.component_bases() {
                    frame_dirs.push((format!("{b}/{FRAMES_SUBDIR}"), "skill".into()));
                    piece_dirs.push(format!("{b}/{PIECES_SUBDIR}"));
                }
            }
        }
        for (dir, origin) in &frame_dirs {
            if self.fs.exists(dir) {
                let (a, p) = self.frames_in(dir, origin, false)?;
                lib.frames.extend(a);
                lib.pending.extend(p);
                lib.sources.push(dir.clone());
            }
        }
        if self.fs.exists(&format!("{PROJECT_DIR}/{MANIFEST_FILE}")) && (pack.is_none() || !frame_dirs.is_empty()) {
            let (a, p) = self.frames_in(PROJECT_DIR, "project", true)?;
            lib.frames.extend(a);
            lib.pending.extend(p);
            lib.sources.push(PROJECT_DIR.into());
        }
        for dir in &piece_dirs {
            if self.fs.exists(dir) {
                lib.pieces.extend(self.pieces_in(dir));
                lib.sources.push(dir.clone());
            }
        }
        // one key, one frame and one piece: the first folder wins
        let mut seen = std::collections::BTreeSet::new();
        lib.frames.retain(|f| seen.insert(f.key.clone()));
        let mut seen = std::collections::BTreeSet::new();
        lib.pieces.retain(|p| seen.insert(p.key.clone()));
        Ok(lib)
    }

    /// The frame decision of one plan: `None` when the model draws the whole icon (the default and the main way).
    pub(crate) fn decide_frame(&self, req: &imagegen_core::plan::PlanRequest, pack: Option<&StylePack>) -> EResult<Option<FramePlan>> {
        let requested = req.frame.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(AUTO);
        if requested == AUTO || requested == MODEL {
            return Ok(None);
        }
        if !framed_family(req.family) {
            return Err(EngineError::invalid(format!("frame is for focus icons, not {}", req.family)));
        }
        if requested == BLANK {
            return Ok(Some(FramePlan {
                key: BLANK.into(),
                origin: None,
                path: None,
                sha256: None,
                place: None,
                piece: None,
                note: "the model draws an empty frame; look at it, then add it to the library with frame add".into(),
            }));
        }
        self.frame_plan_for(requested, None, pack, req.family).map(Some)
    }

    /// The plan for frame `key` of the library, with the piece the subject is (for an import).
    pub(crate) fn frame_plan_for(&self, key: &str, piece: Option<&str>, pack: Option<&StylePack>, family: Family) -> EResult<FramePlan> {
        let lib = self.library(pack, family)?;
        let Some(f) = lib.frame(key) else {
            let keys: Vec<&str> = lib.frames.iter().map(|f| f.key.as_str()).collect();
            return Err(EngineError::invalid(if keys.is_empty() {
                format!("frame {key:?}: no component frames were found (the asset skill's assets/visual_components/{FRAMES_SUBDIR}, a pack's frames, or .hoi4-imagegen/frames); use frame: \"model\"")
            } else {
                format!("the library has no frame {key:?}; its frames are: {}, or model, auto and blank", keys.join(", "))
            }));
        };
        Ok(FramePlan {
            key: f.key.clone(),
            origin: Some(f.origin.clone()),
            path: Some(f.path.clone()),
            sha256: Some(f.sha256.clone()),
            place: Some(f.place),
            piece: piece.map(str::to_string),
            note: format!("the subject goes on the {} frame {} ({})", f.form.as_str(), f.key, f.path),
        })
    }

    /// The `frame` finisher: the subject on the plan's frame.
    pub(crate) fn run_frame_finisher(&self, img: &Rgba, plan: &FramePlan, canvas: [u32; 2]) -> EResult<(Rgba, Value)> {
        let (Some(path), Some(place)) = (&plan.path, plan.place) else {
            return Err(EngineError::invalid("this asset's frame decision names no frame file"));
        };
        let (bytes, frame) = self.read_component(path).map_err(|e| {
            EngineError::refused(
                "frame_missing",
                format!("the frame {path} cannot be read ({}); restore it or plan afresh", e.message),
                RetryClass::RefreshPreconditions,
            )
        })?;
        if plan.sha256.as_deref().is_some_and(|w| !w.eq_ignore_ascii_case(&sha256_hex(&bytes))) {
            return Err(EngineError::refused(
                "frame_changed",
                format!("{path} changed after this icon was planned; plan it again to use the new frame"),
                RetryClass::RefreshPreconditions,
            ));
        }
        let (out, rep) = icon_frame::compose(img, &frame, place, canvas).map_err(|e| {
            EngineError::refused("frame_failed", format!("the subject could not be placed on frame {}: {e}", plan.key), RetryClass::Terminal)
        })?;
        Ok((
            out,
            json!({
                "frame": plan.key,
                "frame_path": path,
                "frame_sha256": plan.sha256,
                "piece": plan.piece,
                "box": place,
                "scale": (rep.scale * 1000.0).round() / 1000.0,
                "fill": (rep.fill * 1000.0).round() / 1000.0,
                "placed": rep.placed,
                "inside_canvas": rep.inside_canvas,
                "problems": rep.problems(),
                "recipe_version": icon_frame::RECIPE_VERSION,
            }),
        ))
    }

    /// `frame list`: the library for a pack (or none) and a contact sheet of every frame with its box and every piece,
    /// written to the user's cache folder.
    pub fn frame_list(&self, pack: Option<&str>) -> EResult<Value> {
        let choice = self.choose_pack(pack)?;
        let lib = self.library(choice.as_ref().map(|c| &c.pack), Family::Focus)?;
        let canvas = self.family_profile(Family::Focus)?.canvas;
        let mut tiles: Vec<Rgba> = Vec::new();
        for f in lib.frames.iter().chain(&lib.pending) {
            if let Ok((_, img)) = self.read_component(&f.path) {
                let mut t = Rgba::new(canvas[0].max(img.width), canvas[1].max(img.height));
                let (fx, fy) = ((t.width - img.width) / 2, (t.height - img.height) / 2);
                t.paste_over(&img, i64::from(fx), i64::from(fy));
                // the placement box in green
                let [x0, y0, x1, y1] = f.place;
                for x in x0..x1 {
                    for y in [y0, y1 - 1] {
                        t.set_px(fx + x, fy + y, [40, 230, 60, 255]);
                    }
                }
                for y in y0..y1 {
                    for x in [x0, x1 - 1] {
                        t.set_px(fx + x, fy + y, [40, 230, 60, 255]);
                    }
                }
                tiles.push(t);
            }
        }
        for p in &lib.pieces {
            if let Ok((_, img)) = self.read_component(&p.path) {
                tiles.push(img);
            }
        }
        let sheet = sheet(&tiles, canvas)?;
        let path = self.project_cache_dir("frames").and_then(|d| {
            let file = d.join(format!("{}.png", pack.unwrap_or("library")));
            std::fs::create_dir_all(&d).and_then(|_| std::fs::write(&file, &sheet)).ok().map(|_| file)
        });
        Ok(json!({
            "frames": lib.frames,
            "pending": lib.pending,
            "pieces": lib.pieces.iter().map(|p| json!({"key": p.key, "path": p.path, "words": p.words})).collect::<Vec<_>>(),
            "sources": lib.sources,
            "component_share": lib.share,
            "sheet": path.map(|p| display_path(&p)),
            "next": "Frames first (the green box is where a subject goes), then the pieces. A frame drawn with frame: \"blank\" joins with frame add; frames.toml in a frames folder sets a key, form, box or weight by hand.",
        }))
    }

    /// `frame add`: put a processed blank frame (made with `frame: "blank"`) into the project's library. Its box is
    /// measured unless given; it is used only once `approve` is true (after a person looked at it).
    pub fn frame_add(&self, artifact_id: &str, key: Option<&str>, place: Option<[u32; 4]>, approve: bool) -> EResult<Value> {
        let a = self.artifact(artifact_id)?;
        if !a.processed || a.family != Family::Focus {
            return Err(EngineError::invalid("frame add takes a processed focus artifact (process the blank frame first)"));
        }
        if a.frame.as_ref().is_none_or(|f| f.key != BLANK) {
            return Err(EngineError::invalid(format!(
                "{artifact_id} was not planned with frame: \"blank\"; a frame for the library is an empty frame"
            )));
        }
        let key = key.map(str::to_string).unwrap_or_else(|| a.asset.clone());
        frameset::check_key(&key)?;
        let (bytes, sha) = self.read_artifact(&a)?;
        let (img, _) = decode(&bytes, Some(&a.path), DecodeLimits::default())?;
        let (place, how) = match place {
            Some(b) => (b, "given"),
            None => framebox::measure(&img).map(|(b, m)| (b, m.as_str())).ok_or_else(|| EngineError::invalid("the frame is empty"))?,
        };
        let manifest_rel = format!("{PROJECT_DIR}/{MANIFEST_FILE}");
        let mut m = match self.fs.read(&manifest_rel, frameset::MAX_MANIFEST_BYTES) {
            Ok(b) => FrameManifest::parse(&String::from_utf8_lossy(&b))?,
            Err(imagegen_core::Error::NotFound(_)) => FrameManifest { schema: 1, frames: vec![] },
            Err(e) => return Err(e.into()),
        };
        let file = format!("{key}.png");
        m.frames.retain(|e| e.file != file);
        m.frames.push(FrameEntry {
            file: file.clone(),
            key: Some(key.clone()),
            form: Some(framebox::guess_form(&img)),
            place: Some(place),
            weight: None,
            approved: approve,
            sha256: Some(sha256_hex(&bytes).to_ascii_lowercase()),
            note: format!("from {artifact_id} ({sha})"),
        });
        m.check()?;
        self.fs.replace(&format!("{PROJECT_DIR}/{file}"), &bytes)?;
        self.fs.replace(&manifest_rel, m.to_toml()?.as_bytes())?;
        Ok(json!({
            "added": key,
            "path": format!("{PROJECT_DIR}/{file}"),
            "box": place,
            "box_from": how,
            "approved": approve,
            "next": if approve { "The frame is in the library: frame list shows it, and sets may pick it." } else { "Look at the frame and its box (frame list), then run frame add again with approve to use it." },
        }))
    }
}

fn sheet(tiles: &[Rgba], canvas: [u32; 2]) -> EResult<Vec<u8>> {
    const GAP: u32 = 6;
    const COLS: u32 = 8;
    let (cw, ch) = tiles.iter().fold((canvas[0], canvas[1]), |a, t| (a.0.max(t.width), a.1.max(t.height)));
    let rows = (tiles.len() as u32).div_ceil(COLS).max(1);
    let mut s = Rgba::filled(GAP + COLS * (cw + GAP), GAP + rows * (ch + GAP), [58, 62, 70, 255]);
    for (i, t) in tiles.iter().enumerate() {
        let (c, r) = (i as u32 % COLS, i as u32 / COLS);
        let (x, y) = (GAP + c * (cw + GAP), GAP + r * (ch + GAP));
        s.paste(&Rgba::filled(cw, ch, [92, 96, 104, 255]), i64::from(x), i64::from(y));
        s.paste_over(t, i64::from(x + (cw - t.width) / 2), i64::from(y + (ch - t.height) / 2));
    }
    Ok(encode_png(&s)?)
}
