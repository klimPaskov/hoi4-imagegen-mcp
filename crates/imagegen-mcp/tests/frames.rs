//! The focus icon component library: frames and pieces found in the asset skill, a piece placed on a frame, the
//! prompts of the frame modes, and the share of a set built from components.
//!
//! SYNTHETIC: the frame (a ring), the piece (a dark block) and the focus tree are drawn or written by the test.
//! Nothing is generated: plans are free and the icons are imports.

mod common;

use common::{openai_harness, Harness};
use imagegen_core::pixels::{encode_png, Rgba};
use serde_json::{json, Value};

const BASE: &str = ".claude/skills/kit/assets/visual_components";

fn write(h: &Harness, rel: &str, bytes: &[u8]) {
    let p = h.root().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn ring() -> Rgba {
    let mut img = Rgba::new(80, 80);
    for y in 0..80u32 {
        for x in 0..80u32 {
            let d = ((f64::from(x) - 39.5).powi(2) + (f64::from(y) - 39.5).powi(2)).sqrt();
            if (28.0..37.0).contains(&d) {
                img.set_px(x, y, [190, 150, 70, 255]);
            }
        }
    }
    img
}

fn block(w: u32, h: u32, rgb: [u8; 3]) -> Rgba {
    let mut img = Rgba::new(w + 6, h + 6);
    for y in 3..3 + h {
        for x in 3..3 + w {
            img.set_px(x, y, [rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    img
}

fn library(h: &Harness) {
    write(h, &format!("{BASE}/focus_backgrounds/focus_background_test_ring.png"), &encode_png(&ring()).unwrap());
    write(h, &format!("{BASE}/focus_backgrounds/contact_sheet.png"), &encode_png(&Rgba::filled(20, 20, [1, 1, 1, 255])).unwrap());
    write(h, &format!("{BASE}/focus_pieces/focus_piece_tank.png"), &encode_png(&block(50, 40, [210, 205, 190])).unwrap());
    write(h, &format!("{BASE}/focus_pieces/focus_piece_dove.png"), &encode_png(&block(40, 40, [220, 220, 220])).unwrap());
}

fn references(h: &Harness, params: Value) -> Result<Value, imagegen_mcp::EngineError> {
    h.engine.references(serde_json::from_value(params).unwrap())
}

fn plan(h: &Harness, frame: Option<&str>) -> Result<imagegen_core::plan::Plan, imagegen_mcp::EngineError> {
    let mut req =
        json!({"family": "focus", "feature_slug": "f", "asset": "a", "brief": "a riveted steel gate", "provider": "openai", "quality": "low"});
    if let Some(f) = frame {
        req["frame"] = json!(f);
    }
    h.engine.plan(serde_json::from_value(req).unwrap())
}

#[test]
fn the_library_is_found_in_the_skill_and_a_piece_goes_on_a_frame() {
    let h = openai_harness();
    library(&h);
    let v = h.engine.frame_list(None).unwrap();
    let frames = v["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 1, "the contact sheet is not a frame: {v:#}");
    assert_eq!(frames[0]["key"], "test_ring");
    assert_eq!(frames[0]["box_from"], "hole");
    assert_eq!(v["pieces"].as_array().unwrap().len(), 2);

    let import = json!({"import": {"path": format!("{BASE}/focus_pieces/focus_piece_tank.png"), "family": "focus", "feature_slug": "f", "asset": "tank_icon", "frame": "test_ring"}});
    let p = h.engine.process(serde_json::from_value(import).unwrap()).unwrap();
    assert_eq!(p["size"], json!([100, 88]), "{p:#}");
    let fin = p["finishers"].as_array().unwrap();
    let frame = fin.iter().find(|f| f["name"] == "frame").expect("the frame step is recorded");
    assert_eq!(frame["detail"]["frame"], "test_ring");
    assert_eq!(frame["detail"]["piece"], "tank");
    assert!(frame["detail"]["frame_sha256"].as_str().is_some_and(|s| s.len() == 64));
    assert_eq!(frame["detail"]["problems"], json!([]), "{frame:#}");
    let png = h.read(p["path"].as_str().unwrap());
    let (img, _) = imagegen_core::pixels::decode(&png, None, Default::default()).unwrap();
    // the 80x80 ring is centred on the canvas (at 10, 4), the piece sits in its middle, the corner stays clear
    assert_eq!(img.px(50, 44)[0], 210);
    assert_eq!(img.px(50, 4 + 8)[3], 255);
    assert_eq!(img.alpha(1, 1), 0);
    // an unknown key names the keys there are
    let bad = json!({"import": {"path": format!("{BASE}/focus_pieces/focus_piece_tank.png"), "family": "focus", "feature_slug": "f", "asset": "x", "frame": "nope"}});
    let e = h.engine.process(serde_json::from_value(bad).unwrap()).unwrap_err();
    assert!(e.message.contains("test_ring"), "{e}");
}

#[test]
fn the_model_path_is_unchanged_and_the_frame_modes_change_only_their_prompt() {
    let h = openai_harness();
    library(&h);
    let none = plan(&h, None).unwrap();
    let model = plan(&h, Some("model")).unwrap();
    let auto = plan(&h, Some("auto")).unwrap();
    assert!(none.frame.is_none() && model.frame.is_none() && auto.frame.is_none());
    assert_eq!(none.prompt.text, model.prompt.text, "frame model is today's prompt");
    assert_eq!(none.prompt.text, auto.prompt.text);
    let on_frame = plan(&h, Some("test_ring")).unwrap();
    assert_eq!(on_frame.frame.as_ref().unwrap().key, "test_ring");
    assert!(on_frame.prompt.text.contains("SUBJECT ONLY") && !on_frame.prompt.text.contains("do not share one frame"), "{}", on_frame.prompt.text);
    let blank = plan(&h, Some("blank")).unwrap();
    assert!(blank.prompt.text.contains("EMPTY frame"), "{}", blank.prompt.text);
    assert!(plan(&h, Some("nope")).is_err());
    // other families take no frame but model
    let req = json!({"family": "decision", "feature_slug": "f", "asset": "a", "brief": "x", "provider": "openai", "frame": "test_ring"});
    assert!(h.engine.plan(serde_json::from_value(req).unwrap()).is_err());
}

#[test]
fn a_set_builds_a_share_of_its_generic_icons_from_components() {
    let h = openai_harness();
    library(&h);
    let names = ["Tank Doctrine", "The Easter Rising", "Gaelic Revival", "Peace in Europe", "Civil Service", "Land Reform"];
    let mut tree = String::from("focus_tree = {\n\tid = t\n");
    let mut loc = String::from("\u{feff}l_english:\n");
    for (i, n) in names.iter().enumerate() {
        tree.push_str(&format!("\tfocus = {{\n\t\tid = TST_f{i}\n\t\ticon = GFX_focus_tst_f{i}\n\t\tx = {i}\n\t\ty = 0\n\t\tcost = 10\n\t}}\n"));
        loc.push_str(&format!(" TST_f{i}:0 \"{n}\"\n"));
    }
    tree.push_str("}\n");
    write(&h, "common/national_focus/t.txt", tree.as_bytes());
    write(&h, "localisation/english/t_l_english.yml", loc.as_bytes());
    let v = h.engine.set(serde_json::from_value(json!({"focus_tree": "common/national_focus/t.txt", "quality": "low"})).unwrap()).unwrap();
    let built: Vec<&Value> = v["icons"].as_array().unwrap().iter().filter(|r| r.get("built_from").is_some()).collect();
    assert!(!built.is_empty() && built.len() <= 2, "a quarter of six, rounded: {v:#}");
    assert!(built.iter().all(|r| r["plan_id"].is_null()), "a built icon has no paid plan");
    assert_eq!(v["planned"].as_u64().unwrap() as usize + built.len(), names.len());
    let off = h.engine.set(serde_json::from_value(json!({"focus_tree": "common/national_focus/t.txt", "component_share": 0.0})).unwrap()).unwrap();
    assert_eq!(off["components"]["built"], 0);
}

#[test]
fn only_a_processed_blank_frame_joins_the_library() {
    let h = openai_harness();
    library(&h);
    let import = json!({"import": {"path": format!("{BASE}/focus_pieces/focus_piece_tank.png"), "family": "focus", "feature_slug": "f", "asset": "tank_icon", "frame": "test_ring"}});
    let p = h.engine.process(serde_json::from_value(import).unwrap()).unwrap();
    let e = h.engine.frame_add(p["artifact_id"].as_str().unwrap(), Some("mine"), None, true).unwrap_err();
    assert!(e.message.contains("blank"), "{e}");
    let e = references(&h, json!({"action": "frame_add"})).unwrap_err();
    assert!(e.message.contains("artifact_id"), "{e}");
}
