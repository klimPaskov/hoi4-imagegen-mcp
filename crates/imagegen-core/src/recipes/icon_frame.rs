//! A subject on a blank frame of the component library (component-built focus icons).
//!
//! The frame is the backdrop layer and the subject goes on top of it, as the asset skill's components are meant to
//! be used: the blank frame is centred on the icon's canvas, the subject is trimmed to its pixels and scaled to fit
//! the frame's placement box ([`FILL`] of it, aspect kept), centred on the box. A pure function of the pixels and the
//! box.

use crate::error::{Error, Result};
use crate::frameset::FRAME_VERSION;
use crate::pixels::Rgba;
use crate::recipes::pillow::alpha_composite;
use crate::resize::resize_lanczos3;

/// Version of the compositing recipe (bump when an output could change).
pub const RECIPE_VERSION: u32 = FRAME_VERSION;
/// Alpha at which a subject pixel counts.
pub const MIN_ALPHA: u8 = 16;
/// Share of the placement box the subject fills on its limiting side.
pub const FILL: f64 = 0.92;
/// The subject is never enlarged more than this (as every icon family's processing).
pub const MAX_UPSCALE: f64 = crate::process::ICON_MAX_UPSCALE;
/// The subject must fill at least this share of the box on its limiting side (the `frame_fill` check).
pub const MIN_FILL: f64 = 0.55;

/// What the compositor measured.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeReport {
    /// Scale applied to the trimmed subject.
    pub scale: f64,
    /// Share of the box the subject's box covers on its limiting side.
    pub fill: f64,
    /// True when the frame and the subject lie inside the canvas.
    pub inside_canvas: bool,
    /// Subject box on the canvas `[x0, y0, x1, y1]`.
    pub placed: [i64; 4],
    /// Where the frame sits on the canvas (its top-left corner).
    pub frame_at: [i64; 2],
}

impl ComposeReport {
    /// Problems found: empty when the subject fills the box and everything stays inside the canvas.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.fill < MIN_FILL {
            out.push(format!(
                "the subject fills only {:.0} percent of the frame's plate (at least {:.0} percent)",
                self.fill * 100.0,
                MIN_FILL * 100.0
            ));
        }
        if !self.inside_canvas {
            out.push(format!("the frame or the subject leaves the canvas (subject at {:?}, frame at {:?})", self.placed, self.frame_at));
        }
        out
    }
}

/// Put `subject` (RGBA, transparent background, any size) on `frame` (a blank frame whose plate is `place`, in the
/// frame's pixels), on a `canvas` the size of the icon.
pub fn compose(subject: &Rgba, frame: &Rgba, place: [u32; 4], canvas: [u32; 2]) -> Result<(Rgba, ComposeReport)> {
    let bbox = subject.alpha_bbox(MIN_ALPHA).ok_or_else(|| Error::Invalid("the subject is empty: nothing to put on a frame".into()))?;
    let trimmed = subject.crop(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1])?;
    let [x0, y0, x1, y1] = place;
    if x0 >= x1 || y0 >= y1 || x1 > frame.width || y1 > frame.height {
        return Err(Error::Invalid(format!("the placement box {place:?} is not inside the {}x{} frame", frame.width, frame.height)));
    }
    let fx = (i64::from(canvas[0]) - i64::from(frame.width)) / 2;
    let fy = (i64::from(canvas[1]) - i64::from(frame.height)) / 2;
    let (bw, bh) = (f64::from(x1 - x0), f64::from(y1 - y0));
    let (sw, sh) = (f64::from(trimmed.width), f64::from(trimmed.height));
    let scale = (bw * FILL / sw).min(bh * FILL / sh).min(MAX_UPSCALE);
    let (nw, nh) = (((sw * scale).round() as u32).max(1), ((sh * scale).round() as u32).max(1));
    let fitted = resize_lanczos3(&trimmed, nw, nh)?;
    let cx = fx as f64 + f64::from(x0 + x1) / 2.0;
    let cy = fy as f64 + f64::from(y0 + y1) / 2.0;
    let px = (cx - f64::from(nw) / 2.0).round() as i64;
    let py = (cy - f64::from(nh) / 2.0).round() as i64;
    let mut out = Rgba::new(canvas[0], canvas[1]);
    alpha_composite(&mut out, frame, fx, fy);
    alpha_composite(&mut out, &fitted, px, py);
    let placed = [px, py, px + i64::from(nw), py + i64::from(nh)];
    let inside = |a: i64, b: i64, c: i64, d: i64| a >= 0 && b >= 0 && c <= i64::from(canvas[0]) && d <= i64::from(canvas[1]);
    let inside_canvas =
        inside(placed[0], placed[1], placed[2], placed[3]) && inside(fx, fy, fx + i64::from(frame.width), fy + i64::from(frame.height));
    let fill = (f64::from(nw) / bw).max(f64::from(nh) / bh);
    Ok((out, ComposeReport { scale, fill, inside_canvas, placed, frame_at: [fx, fy] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 68x68 ring on its own, smaller than the canvas.
    fn ring() -> Rgba {
        let mut img = Rgba::new(68, 68);
        for y in 0..68u32 {
            for x in 0..68u32 {
                let d = ((f64::from(x) - 33.5).powi(2) + (f64::from(y) - 33.5).powi(2)).sqrt();
                if (24.0..31.0).contains(&d) {
                    img.set_px(x, y, [200, 160, 40, 255]);
                }
            }
        }
        img
    }

    fn subject(w: u32, h: u32) -> Rgba {
        let mut s = Rgba::new(w + 20, h + 20);
        for y in 10..10 + h {
            for x in 10..10 + w {
                s.set_px(x, y, [220, 30, 30, 255]);
            }
        }
        s
    }

    #[test]
    fn the_frame_is_centred_and_the_subject_fills_its_box() {
        let (img, rep) = compose(&subject(60, 40), &ring(), [10, 10, 58, 58], [100, 88]).unwrap();
        assert_eq!((img.width, img.height), (100, 88));
        assert_eq!(rep.frame_at, [16, 10]);
        assert!(rep.problems().is_empty(), "{rep:?}");
        // wide subject: the width limits, 48 * 0.92 = 44 px
        assert_eq!(rep.placed[2] - rep.placed[0], 44);
        // a small piece is enlarged at most 1.25 times
        let (_, small) = compose(&subject(10, 10), &ring(), [10, 10, 58, 58], [100, 88]).unwrap();
        assert_eq!(small.placed[2] - small.placed[0], 13);
        assert_eq!(small.problems().len(), 1, "a piece too small for the plate is reported");
        // the subject is on top of the frame, the ring shows around it, the corners stay clear
        assert_eq!(img.px(50, 44)[0], 220);
        assert_eq!(img.px(16 + 33, 10 + 5), [200, 160, 40, 255]);
        assert_eq!(img.alpha(1, 1), 0);
    }

    #[test]
    fn bad_boxes_empty_subjects_and_weak_fits_are_caught() {
        assert!(compose(&subject(10, 10), &ring(), [10, 10, 80, 58], [100, 88]).is_err());
        assert!(compose(&Rgba::new(40, 40), &ring(), [10, 10, 58, 58], [100, 88]).is_err());
        let (_, rep) = compose(&subject(60, 40), &ring(), [10, 10, 58, 58], [100, 88]).unwrap();
        assert_eq!(ComposeReport { fill: 0.3, ..rep.clone() }.problems().len(), 1);
        assert_eq!(ComposeReport { inside_canvas: false, ..rep }.problems().len(), 1);
        // a frame wider than the canvas sticks out
        let (_, wide) = compose(&subject(10, 10), &Rgba::filled(120, 40, [9, 9, 9, 255]), [10, 5, 110, 35], [100, 88]).unwrap();
        assert!(!wide.inside_canvas);
    }

    #[test]
    fn composition_repeats_exactly() {
        let a = compose(&subject(50, 50), &ring(), [10, 10, 58, 58], [100, 88]).unwrap().0;
        let b = compose(&subject(50, 50), &ring(), [10, 10, 58, 58], [100, 88]).unwrap().0;
        assert_eq!(a.pixels_sha256(), b.pixels_sha256());
    }
}
