//! Where a subject goes on a blank focus frame.
//!
//! The asset skill's `visual_components/focus_backgrounds` holds blank frames: a plate, ring or shield with nothing
//! on it. A component-built icon puts a piece on the frame's plate, so each frame needs its placement box. The frames
//! the skill ships are known by their SHA-256 ([`KNOWN`], boxes checked by eye on an enlarged sheet); any other frame,
//! such as one the model drew with `frame: "blank"`, is measured:
//!
//! 1. **Hole.** A ring: the clear pixels its rim encloses (the outside cannot reach them) are the box.
//! 2. **Plate.** The largest smooth region of solid pixels (the luminance range of a 5x5 window under
//!    [`SMOOTH_RANGE`]), opened, closed and filled. A flat painted plate is smooth, wreaths and rims are not.
//! 3. **Fallback.** The solid bounding box shrunk to [`FALLBACK_SHARE`] around its centre.
//!
//! A `frames.toml` entry can always set the box by hand. Pure functions of the pixels.

use crate::frameset::Form;
use crate::pixels::Rgba;

/// A 5x5 window whose luminance range is under this counts as smooth plate.
pub const SMOOTH_RANGE: f64 = 46.0;
/// A measured box must be at least this share of the frame's solid bounding box on each side.
pub const MIN_SIDE_SHARE: f64 = 0.40;
/// The fallback box's share of the solid bounding box.
pub const FALLBACK_SHARE: f64 = 0.62;

/// The blank frames the asset skill ships: `(file stem, SHA-256 of the file, form, box [x0, y0, x1, y1])` in the
/// file's own pixels. The boxes were drawn on a 5x enlarged sheet and sit on each plate inside its rim.
pub const KNOWN: &[(&str, &str, Form, [u32; 4])] = &[
    ("focus_background_circle_ribbon_2", "5bcf53259a04276b1586f6c83f7a35a14ba9d22742ea75fb4802911978db2f95", Form::Round, [24, 13, 79, 73]),
    ("focus_background_communist_wreath", "c58aed1ebe40ab1a81dd74a32deba5ccf0a3b76649e6f3e365e485324bd9f7e1", Form::Round, [24, 18, 78, 71]),
    ("focus_background_flag_emblem", "4b25ca7b9eec6229bfe102288f9bd268c76c81e01ab719df74b09a4c14dbc04c", Form::Shield, [18, 12, 76, 60]),
    ("focus_background_gold_ring", "1b29ce33d3dafd48f7a2a24f5e8885039a0a5a8fea7fd60bb7598bd8b5867307", Form::Round, [9, 8, 59, 59]),
    ("focus_background_kingdom_frame", "8c33f4535b7a87dc4a08c50b1fa8b91fc27cfe919c8efde73912431d1a6b966e", Form::Shield, [31, 28, 71, 69]),
    ("focus_background_plain", "19b9953e6f12b659b86be7150d4bb3d46f93d19d5224558740d5369c18672917", Form::Round, [6, 6, 60, 60]),
    ("focus_background_rugged_frame", "4f5c7cda419107415fa77c1bf2c499399494f877741da36cee614ffc671e04be", Form::Round, [12, 12, 56, 56]),
    ("focus_background_shield_guns", "d76aa72a3560d311a1444b13dd608a871ea613a69890e1ba801dbb0fabd516e0", Form::Shield, [29, 17, 68, 63]),
];

/// The known entry for a file's SHA-256.
pub fn known(sha256: &str) -> Option<(Form, [u32; 4])> {
    KNOWN.iter().find(|k| k.1.eq_ignore_ascii_case(sha256)).map(|k| (k.2, k.3))
}

/// Known frames a set never picks by itself (they stay usable by key): the plain white ball, which a blind review of
/// the first component-built Irish set judged unfinished next to framed icons (2026-11-09).
pub const NOT_PICKED: &[&str] = &["focus_background_plain"];

/// True when the known frame with this SHA-256 is one a set never picks by itself.
pub fn not_picked(sha256: &str) -> bool {
    KNOWN.iter().any(|k| k.1.eq_ignore_ascii_case(sha256) && NOT_PICKED.contains(&k.0))
}

/// How a box was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The clear inside of a ring.
    Hole,
    /// The largest smooth plate.
    Plate,
    /// The shrunk solid box.
    Fallback,
}

impl Method {
    /// Name for output.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Hole => "hole",
            Method::Plate => "plate",
            Method::Fallback => "fallback",
        }
    }
}

fn components(grid: &[bool], w: usize, h: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; grid.len()];
    let mut out = Vec::new();
    for start in 0..grid.len() {
        if !grid[start] || seen[start] {
            continue;
        }
        let mut comp = Vec::new();
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(p) = stack.pop() {
            comp.push(p);
            let (x, y) = (p % w, p / w);
            let mut push = |q: usize| {
                if grid[q] && !seen[q] {
                    seen[q] = true;
                    stack.push(q);
                }
            };
            if x > 0 {
                push(p - 1);
            }
            if x + 1 < w {
                push(p + 1);
            }
            if y > 0 {
                push(p - w);
            }
            if y + 1 < h {
                push(p + w);
            }
        }
        out.push(comp);
    }
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    out
}

fn touches_border(comp: &[usize], w: usize, h: usize) -> bool {
    comp.iter().any(|&p| p % w == 0 || p % w == w - 1 || p / w == 0 || p / w == h - 1)
}

/// One step of dilation (`grow`) or erosion.
fn step(grid: &[bool], w: usize, h: usize, grow: bool) -> Vec<bool> {
    let mut out = grid.to_vec();
    for y in 0..h {
        for x in 0..w {
            let p = y * w + x;
            let n = [(x > 0).then(|| p - 1), (x + 1 < w).then(|| p + 1), (y > 0).then(|| p - w), (y + 1 < h).then(|| p + w)];
            if grow && !grid[p] {
                out[p] = n.iter().flatten().any(|&q| grid[q]);
            } else if !grow && grid[p] {
                out[p] = n.iter().all(|q| q.is_some_and(|q| grid[q]));
            }
        }
    }
    out
}

fn bbox(cells: impl Iterator<Item = usize>, w: usize) -> Option<[u32; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    let mut any = false;
    for p in cells {
        any = true;
        x0 = x0.min(p % w);
        x1 = x1.max(p % w + 1);
        y0 = y0.min(p / w);
        y1 = y1.max(p / w + 1);
    }
    any.then_some([x0 as u32, y0 as u32, x1 as u32, y1 as u32])
}

/// Measure the placement box of a blank frame (see the module notes). `None` for an empty image.
pub fn measure(img: &Rgba) -> Option<([u32; 4], Method)> {
    let (w, h) = (img.width as usize, img.height as usize);
    let n = w * h;
    let solid: Vec<bool> = (0..n).map(|p| img.alpha((p % w) as u32, (p / w) as u32) >= 128).collect();
    let outer = bbox((0..n).filter(|&p| solid[p]), w)?;
    let (ow, oh) = (f64::from(outer[2] - outer[0]), f64::from(outer[3] - outer[1]));
    let big_enough = |b: [u32; 4]| f64::from(b[2] - b[0]) >= ow * MIN_SIDE_SHARE && f64::from(b[3] - b[1]) >= oh * MIN_SIDE_SHARE;
    // 1. the inside of a ring
    let clear: Vec<bool> = solid.iter().map(|s| !s).collect();
    let holes: Vec<usize> = components(&clear, w, h).into_iter().filter(|c| !touches_border(c, w, h)).flatten().collect();
    if let Some(b) = bbox(holes.iter().copied(), w).filter(|b| big_enough(*b)) {
        return Some((b, Method::Hole));
    }
    // 2. the largest smooth plate
    let lum: Vec<f64> = (0..n)
        .map(|p| {
            let px = img.px((p % w) as u32, (p / w) as u32);
            (f64::from(px[0]) + f64::from(px[1]) + f64::from(px[2])) / 3.0
        })
        .collect();
    let mut smooth = vec![false; n];
    for y in 2..h.saturating_sub(2) {
        for x in 2..w.saturating_sub(2) {
            if !solid[y * w + x] {
                continue;
            }
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for dy in 0..5 {
                for dx in 0..5 {
                    let v = lum[(y + dy - 2) * w + x + dx - 2];
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            smooth[y * w + x] = hi - lo < SMOOTH_RANGE;
        }
    }
    let mut g = smooth;
    for grow in [false, false, true, true] {
        g = step(&g, w, h, grow);
    }
    if let Some(c) = components(&g, w, h).into_iter().next() {
        let mut m = vec![false; n];
        c.iter().for_each(|&p| m[p] = true);
        for grow in [true, true, false, false] {
            m = step(&m, w, h, grow);
        }
        if let Some(b) = bbox((0..n).filter(|&p| m[p]), w).filter(|b| big_enough(*b)) {
            return Some((b, Method::Plate));
        }
    }
    // 3. the shrunk solid box
    let (cx, cy) = (f64::from(outer[0] + outer[2]) / 2.0, f64::from(outer[1] + outer[3]) / 2.0);
    let (hw, hh) = (ow * FALLBACK_SHARE / 2.0, oh * FALLBACK_SHARE / 2.0);
    Some(([(cx - hw).round() as u32, (cy - hh).round() as u32, (cx + hw).round() as u32, (cy + hh).round() as u32], Method::Fallback))
}

/// The form of a frame from its outline: a shield narrows towards the bottom, anything else counts as round.
pub fn guess_form(img: &Rgba) -> Form {
    let Some([_, y0, _, y1]) = img.alpha_bbox(128) else { return Form::Other };
    let row = |y: u32| (0..img.width).filter(|&x| img.alpha(x, y) >= 128).count() as f64;
    let at = |t: f64| row(((f64::from(y0) + f64::from(y1 - y0 - 1) * t).round() as u32).min(img.height - 1));
    if at(0.9) < at(0.3) * 0.55 {
        Form::Shield
    } else {
        Form::Round
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> Rgba {
        let mut img = Rgba::new(68, 68);
        for y in 0..68u32 {
            for x in 0..68u32 {
                let d = ((f64::from(x) - 33.5).powi(2) + (f64::from(y) - 33.5).powi(2)).sqrt();
                if (24.0..31.0).contains(&d) {
                    img.set_px(x, y, [170, 130, 60, 255]);
                }
            }
        }
        img
    }

    /// A flat red disc inside a busy wreath of alternating pixels.
    fn plate() -> Rgba {
        let mut img = Rgba::new(100, 88);
        for y in 0..88u32 {
            for x in 0..100u32 {
                let d = ((f64::from(x) - 49.5).powi(2) + (f64::from(y) - 43.5).powi(2)).sqrt();
                if d < 28.0 {
                    img.set_px(x, y, [140, 50, 50, 255]);
                } else if d < 40.0 {
                    let v = if (x + y) % 2 == 0 { 230 } else { 60 };
                    img.set_px(x, y, [v, v, 40, 255]);
                }
            }
        }
        img
    }

    #[test]
    fn a_ring_is_measured_by_its_hole() {
        let (b, m) = measure(&ring()).unwrap();
        assert_eq!(m, Method::Hole);
        assert!((44..=50).contains(&(b[2] - b[0])), "{b:?}");
    }

    #[test]
    fn a_plate_is_measured_by_its_smooth_disc() {
        let (b, m) = measure(&plate()).unwrap();
        assert_eq!(m, Method::Plate);
        assert!((46..=58).contains(&(b[2] - b[0])), "{b:?}");
        assert!(b[0] >= 18 && b[2] <= 82, "{b:?}");
    }

    #[test]
    fn a_busy_badge_falls_back_and_empty_is_none() {
        let mut img = Rgba::new(40, 40);
        for y in 0..40u32 {
            for x in 0..40u32 {
                let v = if (x + y) % 2 == 0 { 250 } else { 10 };
                img.set_px(x, y, [v, v, v, 255]);
            }
        }
        assert_eq!(measure(&img).unwrap().1, Method::Fallback);
        assert!(measure(&Rgba::new(10, 10)).is_none());
    }

    #[test]
    fn known_frames_are_found_by_hash() {
        assert_eq!(known("5BCF53259A04276B1586F6C83F7A35A14BA9D22742EA75FB4802911978DB2F95").unwrap().0, Form::Round);
        assert!(known("00").is_none());
        for (_, sha, _, b) in KNOWN {
            assert_eq!(sha.len(), 64);
            assert!(b[0] < b[2] && b[1] < b[3]);
        }
    }

    #[test]
    fn shields_narrow_at_the_bottom() {
        let mut img = Rgba::new(60, 68);
        for y in 0..68u32 {
            let half = if y < 40 { 28.0 } else { 28.0 * (1.0 - f64::from(y - 40) / 28.0) };
            for x in 0..60u32 {
                if (f64::from(x) - 29.5).abs() < half {
                    img.set_px(x, y, [200, 200, 200, 255]);
                }
            }
        }
        assert_eq!(guess_form(&img), Form::Shield);
        assert_eq!(guess_form(&ring()), Form::Round);
    }
}
