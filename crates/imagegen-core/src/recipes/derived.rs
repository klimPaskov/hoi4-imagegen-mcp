//! Companion images the game looks up next to a family's main sprite, derived from the main art.
//!
//! Each derivation was fitted to the installed game (HOI4 1.19.4.1) and none is exact: vanilla draws these
//! companions by hand, so a derived one is a faithful stand-in, not a copy of a vanilla process.
//!
//! | Kind | Families | What vanilla shows | Derivation |
//! |---|---|---|---|
//! | [`DeriveKind::SmallEmblem`] | division emblem | `division_templates_small/` 30x12: the large emblem fitted to about 0.4 scale, light grey (values 124 to 211), with a black outline | fit the subject to 28x10 at most 30/76 scale, remap its luminance to 124..211, add a black outline at 20 percent of the neighbouring alpha |
//! | [`DeriveKind::TextIcon`] | on-map counter | `gfx/texticons/unit_*_icon_small.dds`: the on-map strip, frame 1 tinted green, frame 2 (the NATO box) unchanged in 22 of 34 pairs | frame 1 RGB multiplied by the least-squares tint (0.573, 0.735, 0.573), frame 2 copied |
//! | [`DeriveKind::Miniature`] | faction logo | `*_miniature` 32x32: frame 1's emblem fitted edge to edge (median bounding box 32 px) | frame 1's subject fitted to 32x32 |
//! | [`DeriveKind::MapMedallion`] | operation card | `map/<name>_map.dds` 48x48: the picture's centre at 0.6 scale in a round, bevelled gold ring | the central 64 px of the card in the ring of [`crate::recipes::operation_card`] |
//! | [`DeriveKind::PhaseSmall`] | operation phase picture | `phases_small/<name>_small.dds` 59x58 (71 files): the phase photograph's central square (cropped by hand, 0.85 of its height in the median) in a disc of radius 22.9 centred at (30, 29), its edge darkened to 0.50, 0.69 and 0.92 of the inner value over the outer three pixels, a little lighter (1.03 to 1.10) three to eight pixels in | the central square of the processed phase card in that disc, with that edge ([`phase_small`]) |
//! | [`DeriveKind::HalfSize`] | medal (`_large` to the small strip) | medals: the small strip is the large one downscaled (median MAE 2.6 against Lanczos) | Lanczos3 to half size, frame by frame |
//! | [`DeriveKind::LandmarkSmall`] | landmark (large to `_small`) | `buildings/*_icon_small` 64x32: a flat orange pictogram, yellow-orange at the top to deep orange at the bottom, with a brown edge and a faint black rim, in a box of 26x19 inside 3 px at the sides, 6 at the top and 7 at the bottom | both frames cropped together, fitted into that box, then recoloured on vanilla's orange ramp ([`LANDMARK_SMALL`]) |

use crate::error::{Error, Result};
use crate::pixels::{over, Rgba};
use crate::resize::resize_lanczos3;

/// Size of a small division emblem.
pub const SMALL_EMBLEM: [u32; 2] = [30, 12];
/// Largest scale from the large emblem: the canvas width ratio, 30/76 (vanilla's median is 0.40).
pub const SMALL_EMBLEM_MAX_SCALE: f64 = 30.0 / 76.0;
/// Luminance range of a small emblem's opaque pixels (vanilla deciles 10 and 90 of all 100 files).
pub const SMALL_EMBLEM_GREY: [f64; 2] = [124.0, 211.0];
/// Outline alpha as a share of the brightest neighbouring alpha (vanilla's outline is 36 to 54 beside opaque pixels).
pub const SMALL_EMBLEM_OUTLINE: f64 = 0.2;
/// Text icon tint of frame 1, fitted by least squares on the opaque pixels of the 34 vanilla pairs whose on-map
/// counter and text icon are both 60x12.
pub const TEXT_ICON_TINT: [f64; 3] = [0.573, 0.735, 0.573];
/// Size of a faction logo miniature.
pub const MINIATURE: [u32; 2] = [32, 32];

/// How a companion is made from the main art.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeriveKind {
    /// The 30x12 grey division emblem.
    SmallEmblem,
    /// The green text icon of an on-map counter strip.
    TextIcon,
    /// The 32x32 faction logo miniature from frame 1.
    Miniature,
    /// Every frame at half size.
    HalfSize,
    /// The flat orange pictogram strip of a small landmark.
    LandmarkSmall,
    /// The 48x48 map medallion of an operation card: the card's centre in a gold ring.
    MapMedallion,
    /// The 59x58 round phase icon of an operation phase picture.
    PhaseSmall,
}

impl DeriveKind {
    /// Stable name recorded with the companion.
    pub fn name(self) -> &'static str {
        match self {
            DeriveKind::SmallEmblem => "small_emblem",
            DeriveKind::TextIcon => "text_icon",
            DeriveKind::Miniature => "miniature",
            DeriveKind::HalfSize => "half_size",
            DeriveKind::LandmarkSmall => "landmark_small",
            DeriveKind::MapMedallion => "map_medallion",
            DeriveKind::PhaseSmall => "phase_small",
        }
    }
}

/// Make one companion from `main`, a strip of `frames` frames.
pub fn derive(kind: DeriveKind, main: &Rgba, frames: u32) -> Result<Rgba> {
    match kind {
        DeriveKind::SmallEmblem => small_emblem(main),
        DeriveKind::TextIcon => text_icon(main, frames),
        DeriveKind::Miniature => miniature(main, frames),
        DeriveKind::HalfSize => half_size(main, frames),
        DeriveKind::LandmarkSmall => landmark_small(main, frames),
        DeriveKind::MapMedallion => crate::recipes::operation_card::map_medallion(main),
        DeriveKind::PhaseSmall => phase_small(main),
    }
}

/// Vanilla's small landmark ramp (HOI4 1.19.4.1, frame 1 of the 23 `gfx/interface/buildings/*_icon_small.dds` strips
/// of 64x32): the median colour of the inside pixels whose luminance lies within 8 of 48, 64 ... 192, extended
/// linearly to black below 48 where vanilla has almost no pixels.
pub const LANDMARK_RAMP: [[u8; 3]; 13] = [
    [0, 0, 0],
    [28, 14, 3],
    [57, 28, 6],
    [85, 42, 9],
    [102, 55, 11],
    [133, 64, 13],
    [154, 82, 17],
    [182, 94, 20],
    [243, 102, 19],
    [246, 117, 23],
    [247, 141, 30],
    [249, 162, 36],
    [250, 182, 42],
];

/// The small landmark look: inside luminance deciles 1, 120, 135 ... 193 (median 148, five bands top to bottom
/// 185, 172, 160, 148, 139), edge luminance 103 (an orange-brown), and alpha 39 one pixel outside the shape.
pub const LANDMARK_SMALL: crate::recipes::raid::GradientStyle = crate::recipes::raid::GradientStyle {
    ramp: &LANDMARK_RAMP,
    quantiles: [1.0, 120.0, 135.0, 139.0, 143.0, 148.0, 153.0, 158.0, 165.0, 172.0, 193.0],
    outline: [166, 87, 18],
    height_weight: 0.5,
    rim_alpha: 39,
};

/// The box a small landmark's pictogram is fitted into, `[x0, y0, x1, y1)` (vanilla's median margins: 3 px at the
/// sides, 6 at the top, 7 at the bottom of the 32x32 frame).
pub const LANDMARK_SMALL_BOX: [u32; 4] = [3, 6, 29, 25];

/// The small landmark strip: frame 1's building fitted into [`LANDMARK_SMALL_BOX`] of a 32x32 frame, every other frame
/// moved and scaled the same way (so the ruin keeps its size and place against the intact building, and its flames
/// may rise above the box as vanilla's do), then recoloured as vanilla's flat orange pictograms.
pub fn landmark_small(strip: &Rgba, frames: u32) -> Result<Rgba> {
    let parts = strip.split_frames(frames.max(1))?;
    let b1 = parts[0].alpha_bbox(16).ok_or_else(|| Error::Image("the landmark's frame 1 has no visible subject".into()))?;
    let [x0, y0, x1, y1] = LANDMARK_SMALL_BOX;
    let scale = (f64::from(x1 - x0) / f64::from(b1[2] - b1[0])).min(f64::from(y1 - y0) / f64::from(b1[3] - b1[1]));
    let (fw, fh) = (parts[0].width, parts[0].height);
    let (tw, th) = (((f64::from(fw) * scale).round() as u32).max(1), ((f64::from(fh) * scale).round() as u32).max(1));
    // where frame 1's box lands: centred in the vanilla box
    let ox = f64::from(x0) + (f64::from(x1 - x0) - f64::from(b1[2] - b1[0]) * scale) / 2.0 - f64::from(b1[0]) * scale;
    let oy = f64::from(y0) + (f64::from(y1 - y0) - f64::from(b1[3] - b1[1]) * scale) / 2.0 - f64::from(b1[1]) * scale;
    let mut out = Vec::with_capacity(parts.len());
    for p in &parts {
        let small = resize_lanczos3(p, tw, th)?;
        let mut frame = Rgba::new(32, 32);
        frame.paste(&small, ox.round() as i64, oy.round() as i64);
        let solid = frame.data.chunks_exact(4).filter(|p| p[3] >= 128).count();
        out.push(if solid >= 4 { crate::recipes::raid::gradient_map(&frame, &LANDMARK_SMALL)? } else { frame });
    }
    Rgba::join_frames(&out)
}

/// The subject of `img` (alpha of at least 16), resized by `scale` within `max` and centred on a `canvas`.
fn fit_centred(img: &Rgba, canvas: [u32; 2], max: [u32; 2], scale_cap: Option<f64>) -> Result<Rgba> {
    let b = img.alpha_bbox(16).ok_or_else(|| Error::Image("the art has no visible subject to derive a companion from".into()))?;
    let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
    let subject = img.crop(b[0], b[1], bw, bh)?;
    let mut scale = (f64::from(max[0]) / f64::from(bw)).min(f64::from(max[1]) / f64::from(bh));
    if let Some(cap) = scale_cap {
        scale = scale.min(cap);
    }
    let w = ((f64::from(bw) * scale).round() as u32).clamp(1, max[0]);
    let h = ((f64::from(bh) * scale).round() as u32).clamp(1, max[1]);
    let resized = resize_lanczos3(&subject, w, h)?;
    let mut out = Rgba::new(canvas[0], canvas[1]);
    out.paste(&resized, i64::from((canvas[0] - w) / 2), i64::from((canvas[1] - h) / 2));
    Ok(out)
}

fn luminance(p: [u8; 4]) -> f64 {
    0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])
}

/// The 30x12 small division emblem: the subject fitted to 28x10 (at most 30/76 scale), its luminance remapped to
/// vanilla's light grey range and a black outline around it.
pub fn small_emblem(large: &Rgba) -> Result<Rgba> {
    let fitted = fit_centred(large, SMALL_EMBLEM, [SMALL_EMBLEM[0] - 2, SMALL_EMBLEM[1] - 2], Some(SMALL_EMBLEM_MAX_SCALE))?;
    // deciles 10 and 90 of the opaque pixels' luminance, mapped onto vanilla's range
    let mut lum: Vec<f64> = fitted.data.chunks_exact(4).filter(|p| p[3] >= 128).map(|p| luminance([p[0], p[1], p[2], p[3]])).collect();
    lum.sort_by(f64::total_cmp);
    let (lo, hi) = match lum.len() {
        0 => (0.0, 0.0),
        n => (lum[(n - 1) / 10], lum[(n - 1) * 9 / 10]),
    };
    let [g0, g1] = SMALL_EMBLEM_GREY;
    let mut grey = fitted.clone();
    for p in grey.data.chunks_exact_mut(4) {
        if p[3] == 0 {
            p[..3].copy_from_slice(&[0, 0, 0]);
            continue;
        }
        let l = luminance([p[0], p[1], p[2], p[3]]);
        let v = if hi - lo < 1.0 { (g0 + g1) / 2.0 } else { g0 + (l - lo) * (g1 - g0) / (hi - lo) };
        let v = v.round().clamp(0.0, 255.0) as u8;
        p[..3].copy_from_slice(&[v, v, v]);
    }
    // black outline under the grey subject
    let (w, h) = (grey.width, grey.height);
    let mut out = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
                    if (dx != 0 || dy != 0) && nx >= 0 && ny >= 0 && nx < i64::from(w) && ny < i64::from(h) {
                        m = m.max(grey.alpha(nx as u32, ny as u32));
                    }
                }
            }
            let outline = [0, 0, 0, (f64::from(m) * SMALL_EMBLEM_OUTLINE).round() as u8];
            out.set_px(x, y, over(grey.px(x, y), outline));
        }
    }
    Ok(out)
}

/// The green text icon of an on-map counter strip: frame 1 tinted, the other frames copied.
pub fn text_icon(onmap: &Rgba, frames: u32) -> Result<Rgba> {
    let mut parts = onmap.split_frames(frames.max(1))?;
    for p in parts[0].data.chunks_exact_mut(4) {
        for (c, t) in p[..3].iter_mut().zip(TEXT_ICON_TINT) {
            *c = (f64::from(*c) * t).round().clamp(0.0, 255.0) as u8;
        }
    }
    Rgba::join_frames(&parts)
}

/// The 32x32 miniature: frame 1's subject fitted edge to edge.
pub fn miniature(strip: &Rgba, frames: u32) -> Result<Rgba> {
    let first = strip.split_frames(frames.max(1))?.swap_remove(0);
    fit_centred(&first, MINIATURE, MINIATURE, None)
}

/// Canvas of an operation phase icon (`icon = GFX_phase_<x>_small`).
pub const PHASE_SMALL: [u32; 2] = [59, 58];
/// Centre of its disc in pixels (vanilla's medians over the 71 files: 29.95 and 29.0).
pub const PHASE_SMALL_CENTRE: [f64; 2] = [30.0, 29.0];
/// Radius of its disc (median 22.88, 21.87 to 22.99).
pub const PHASE_SMALL_RADIUS: f64 = 22.9;
/// Side of the picture's central square shown in the disc, as a share of the picture's shorter side. Vanilla crops
/// each icon by hand (the best crop per pair runs from 0.3 to 1.0, median 0.85). Of the fixed crops the whole shorter
/// side matches best (the grey of the inner disc correlates 0.32 with vanilla's in the median, 0.28 at 0.9, 0.24 at
/// 0.85), but on a finished phase card it shows the card's light paper border at the top and bottom, so 0.9 it is.
pub const PHASE_SMALL_CROP: f64 = 0.9;
/// The edge: (depth inside the edge in pixels, value relative to the inner 60 percent of the radius), the medians of
/// vanilla's one-pixel rings.
pub const PHASE_SMALL_EDGE: [(f64, f64); 9] =
    [(0.5, 0.504), (1.5, 0.691), (2.5, 0.918), (3.5, 1.034), (4.5, 1.076), (5.5, 1.095), (6.5, 1.1), (7.5, 1.077), (9.5, 1.0)];

/// Accepted mean value (HSV) of a phase icon's opaque pixels: vanilla's 25th and 75th percentiles over the 71 files
/// (median 0.452). A disc outside gets a gamma to the nearer end.
pub const PHASE_SMALL_VALUE: (f64, f64) = (0.38, 0.531);
/// Largest spread of value of a phase icon's opaque pixels: vanilla's 75th percentile (median 0.281). A harsher disc is
/// compressed toward its mean.
pub const PHASE_SMALL_SPREAD_MAX: f64 = 0.314;

/// Bring the opaque pixels' value into vanilla's range: the spread down to [`PHASE_SMALL_SPREAD_MAX`] about the mean,
/// then a gamma to the nearer end of [`PHASE_SMALL_VALUE`]. Hue and saturation are kept (each pixel's RGB is scaled).
fn phase_tone(img: &mut Rgba) {
    let vals: Vec<f64> = img.data.chunks_exact(4).filter(|p| p[3] > 204).map(|p| f64::from(p[0].max(p[1]).max(p[2])) / 255.0).collect();
    if vals.is_empty() {
        return;
    }
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    let spread = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
    let k = if spread > PHASE_SMALL_SPREAD_MAX { PHASE_SMALL_SPREAD_MAX / spread } else { 1.0 };
    let squeezed = |v: f64| (mean + (v - mean) * k).clamp(0.0, 1.0);
    let mean_at = |g: f64| vals.iter().map(|v| squeezed(*v).powf(g)).sum::<f64>() / n;
    let now = mean_at(1.0);
    let want = now.clamp(PHASE_SMALL_VALUE.0, PHASE_SMALL_VALUE.1);
    let mut gamma = 1.0;
    if (now - want).abs() > 1e-4 {
        // the mean falls as gamma rises
        let (mut lo, mut hi) = if now < want { (0.2, 1.0) } else { (1.0, 5.0) };
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if mean_at(mid) > want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        gamma = (lo + hi) / 2.0;
    }
    if k >= 1.0 && (now - want).abs() <= 1e-4 {
        return;
    }
    for p in img.data.chunks_exact_mut(4).filter(|p| p[3] > 0) {
        let v = f64::from(p[0].max(p[1]).max(p[2])) / 255.0;
        if v <= 1e-6 {
            continue;
        }
        let scale = squeezed(v).powf(gamma) / v;
        for c in p.iter_mut().take(3) {
            *c = (f64::from(*c) * scale).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn phase_edge_at(depth: f64) -> f64 {
    let e = &PHASE_SMALL_EDGE;
    if depth <= e[0].0 {
        return e[0].1;
    }
    for k in 0..e.len() - 1 {
        if depth <= e[k + 1].0 {
            let t = (depth - e[k].0) / (e[k + 1].0 - e[k].0);
            return e[k].1 + (e[k + 1].1 - e[k].1) * t;
        }
    }
    1.0
}

/// The 59x58 phase icon: the central square of the phase picture (the processed card) in vanilla's disc,
/// anti-aliased by 4x4 supersampling, then each one-pixel ring scaled to vanilla's darkened edge and the lighter band
/// inside it, relative to this picture's own inner disc. The picture keeps its
/// own colours: the phase card already gave it vanilla's sepia.
pub fn phase_small(picture: &Rgba) -> Result<Rgba> {
    let short = picture.width.min(picture.height);
    let side = ((f64::from(short) * PHASE_SMALL_CROP).round() as u32).clamp(1, short);
    if side < 8 {
        return Err(Error::Image("the phase picture is too small for a phase icon".into()));
    }
    let centre = picture.crop((picture.width - side) / 2, (picture.height - side) / 2, side, side)?;
    let disc = (PHASE_SMALL_RADIUS * 2.0).ceil() as u32 + 2;
    let pic = resize_lanczos3(&centre, disc, disc)?;
    let [cx, cy] = PHASE_SMALL_CENTRE;
    let r = PHASE_SMALL_RADIUS;
    let half = f64::from(disc) / 2.0;
    let mut out = Rgba::new(PHASE_SMALL[0], PHASE_SMALL[1]);
    const SS: u32 = 4;
    for y in 0..PHASE_SMALL[1] {
        for x in 0..PHASE_SMALL[0] {
            let (mut acc, mut aa) = ([0f64; 3], 0f64);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = f64::from(x) + (f64::from(sx) + 0.5) / f64::from(SS) - cx;
                    let py = f64::from(y) + (f64::from(sy) + 0.5) / f64::from(SS) - cy;
                    let d = px.hypot(py);
                    if d > r {
                        continue;
                    }
                    let (ix, iy) =
                        ((px + half).floor().clamp(0.0, f64::from(disc - 1)) as u32, (py + half).floor().clamp(0.0, f64::from(disc - 1)) as u32);
                    let p = pic.px(ix, iy);
                    // a clear pixel of the card (its tilted corners) shows as the card's paper, not as a hole
                    let a = f64::from(p[3]) / 255.0;
                    for (c, slot) in acc.iter_mut().enumerate() {
                        *slot += f64::from(p[c]) * a + 200.0 * (1.0 - a);
                    }
                    aa += 1.0;
                }
            }
            if aa > 0.0 {
                let c = acc.map(|v| (v / aa).round().clamp(0.0, 255.0) as u8);
                out.set_px(x, y, [c[0], c[1], c[2], (aa / f64::from(SS * SS) * 255.0).round() as u8]);
            }
        }
    }
    // the tone: vanilla's value range and contrast, before the edge is measured against the inner disc
    phase_tone(&mut out);
    // the edge: each one-pixel ring is scaled to vanilla's value relative to the inner 60 percent of the radius,
    // measured on this picture, so a picture that is already dark or light at its rim ends up like vanilla's
    let depth = |x: u32, y: u32| r - (f64::from(x) + 0.5 - cx).hypot(f64::from(y) + 0.5 - cy);
    let value = |p: [u8; 4]| f64::from(p[0].max(p[1]).max(p[2])) / 255.0;
    let rings = PHASE_SMALL_EDGE.len();
    let (mut inner, mut sums) = ((0.0f64, 0.0f64), vec![(0.0f64, 0.0f64); rings]);
    for y in 0..PHASE_SMALL[1] {
        for x in 0..PHASE_SMALL[0] {
            let p = out.px(x, y);
            if p[3] == 0 {
                continue;
            }
            let t = depth(x, y);
            if t >= 0.4 * r {
                inner.0 += value(p);
                inner.1 += 1.0;
            }
            if t >= 0.0 && (t.floor() as usize) < rings {
                let b = t.floor() as usize;
                sums[b].0 += value(p);
                sums[b].1 += 1.0;
            }
        }
    }
    let inner_mean = if inner.1 > 0.0 { inner.0 / inner.1 } else { 0.0 };
    if inner_mean > 1e-3 {
        // the factor of each ring: vanilla's ratio over this picture's
        let factors: Vec<f64> = (0..rings)
            .map(|b| {
                let (sum, n) = sums[b];
                if n <= 0.0 || sum <= 1e-6 {
                    return 1.0;
                }
                (phase_edge_at(b as f64 + 0.5) * inner_mean / (sum / n)).clamp(0.2, 1.6)
            })
            .collect();
        for y in 0..PHASE_SMALL[1] {
            for x in 0..PHASE_SMALL[0] {
                let p = out.px(x, y);
                if p[3] == 0 {
                    continue;
                }
                let t = depth(x, y).max(0.0);
                let b = t.floor() as usize;
                if b >= rings {
                    continue;
                }
                let k = factors[b];
                let c = [0, 1, 2].map(|i| (f64::from(p[i]) * k).round().clamp(0.0, 255.0) as u8);
                out.set_px(x, y, [c[0], c[1], c[2], p[3]]);
            }
        }
    }
    Ok(out)
}

/// Every frame at half size (Lanczos3).
pub fn half_size(strip: &Rgba, frames: u32) -> Result<Rgba> {
    let parts = strip.split_frames(frames.max(1))?;
    let mut out = Vec::with_capacity(parts.len());
    for p in &parts {
        if p.width < 2 || p.height < 2 || p.width % 2 != 0 || p.height % 2 != 0 {
            return Err(Error::Image(format!("a {}x{} frame has no exact half size", p.width, p.height)));
        }
        out.push(resize_lanczos3(p, p.width / 2, p.height / 2)?);
    }
    Rgba::join_frames(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disc of a phase icon: centre and radius from the alpha, and the value of the outer three one-pixel rings
    /// relative to the inner 60 percent of the radius.
    fn phase_disc(img: &Rgba) -> ([f64; 3], [f64; 3]) {
        let (mut cx, mut cy, mut a) = (0.0, 0.0, 0.0);
        for y in 0..img.height {
            for x in 0..img.width {
                let al = f64::from(img.alpha(x, y)) / 255.0;
                cx += (f64::from(x) + 0.5) * al;
                cy += (f64::from(y) + 0.5) * al;
                a += al;
            }
        }
        let (cx, cy, r) = (cx / a, cy / a, (a / std::f64::consts::PI).sqrt());
        let (mut inner, mut rings) = ((0.0, 0.0), [(0.0, 0.0); 3]);
        for y in 0..img.height {
            for x in 0..img.width {
                let p = img.px(x, y);
                if p[3] < 204 {
                    continue;
                }
                let v = f64::from(p[0].max(p[1]).max(p[2])) / 255.0;
                let d = (f64::from(x) + 0.5 - cx).hypot(f64::from(y) + 0.5 - cy);
                if d < 0.6 * r {
                    inner.0 += v;
                    inner.1 += 1.0;
                }
                let k = (r - d).floor();
                if (0.0..3.0).contains(&k) {
                    rings[k as usize].0 += v;
                    rings[k as usize].1 += 1.0;
                }
            }
        }
        let im = inner.0 / inner.1;
        ([cx, cy, r], rings.map(|(s, n)| if n > 0.0 { s / n / im } else { 0.0 }))
    }

    /// A flat mid-grey picture becomes vanilla's disc: 59x58, centred at (30, 29), radius 22.9, clear outside, with
    /// the darkened edge. Deterministic, and a picture too small for it is refused.
    #[test]
    fn a_phase_picture_becomes_vanillas_round_phase_icon() {
        let pic = Rgba::filled(210, 176, [140, 120, 100, 255]);
        let out = phase_small(&pic).unwrap();
        assert_eq!((out.width, out.height), (59, 58));
        let (disc, rings) = phase_disc(&out);
        assert!((disc[0] - 30.0).abs() < 0.2 && (disc[1] - 29.0).abs() < 0.2 && (disc[2] - 22.9).abs() < 0.3, "{disc:?}");
        assert!(rings[0] < 0.6 && rings[1] < 0.8 && rings[2] > 0.85, "{rings:?}");
        assert_eq!(out.alpha(0, 0), 0);
        assert_eq!(out.alpha(30, 29), 255);
        assert_eq!(out, phase_small(&pic).unwrap());
        assert!(phase_small(&Rgba::filled(6, 6, [9, 9, 9, 255])).is_err());
        assert_eq!(derive(DeriveKind::PhaseSmall, &pic, 1).unwrap(), out);
    }

    /// A bright, washed-out picture (a light paper close-up) and a harsh black-and-white one come out in vanilla's
    /// value range and contrast: mean value 0.38 to 0.531, spread at most 0.314, each within what the edge rings add.
    #[test]
    fn a_bright_or_harsh_picture_gets_vanillas_tone() {
        let stats = |img: &Rgba| {
            let v: Vec<f64> = img.data.chunks_exact(4).filter(|p| p[3] > 204).map(|p| f64::from(p[0].max(p[1]).max(p[2])) / 255.0).collect();
            let m = v.iter().sum::<f64>() / v.len() as f64;
            (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt())
        };
        let mut bright = Rgba::new(210, 176);
        let mut harsh = Rgba::new(210, 176);
        for y in 0..176u32 {
            for x in 0..210u32 {
                let t = ((x / 12 + y / 12) % 2) as u8;
                bright.set_px(x, y, [235 - t * 30, 225 - t * 30, 205 - t * 30, 255]);
                harsh.set_px(x, y, if t == 1 { [250, 248, 240, 255] } else { [12, 10, 8, 255] });
            }
        }
        for (name, pic) in [("bright", bright), ("harsh", harsh)] {
            let (m0, s0) = stats(&phase_small(&pic).unwrap());
            eprintln!("{name}: mean value {m0:.3}, spread {s0:.3}");
            assert!((PHASE_SMALL_VALUE.0 - 0.05..=PHASE_SMALL_VALUE.1 + 0.05).contains(&m0), "{name} mean {m0}");
            assert!(s0 <= PHASE_SMALL_SPREAD_MAX + 0.06, "{name} spread {s0}");
        }
    }

    /// Made from vanilla's own 210x176 phase pictures, the icons match vanilla's 59x58 ones in disc and edge, and
    /// show the same picture (the grey of the disc's middle correlates with vanilla's).
    #[test]
    fn phase_icons_from_vanillas_pictures_match_vanillas_phase_icons() {
        let g = std::env::var("HOI4_GAME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| "C:/Program Files (x86)/Steam/steamapps/common/Hearts of Iron IV".into());
        let dir = g.join("gfx/interface/operations");
        let Ok(rd) = std::fs::read_dir(dir.join("phases_small")) else {
            eprintln!("SKIPPED phase icons vs vanilla: Hearts of Iron IV not found (set HOI4_GAME_DIR); UNAVAILABLE in this run");
            return;
        };
        let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let (mut corr, mut n) = (Vec::new(), 0usize);
        let (mut rings_v, mut rings_o): (Vec<[f64; 3]>, Vec<[f64; 3]>) = (Vec::new(), Vec::new());
        for f in &names {
            let Some(stem) = f.strip_suffix("_small.dds") else { continue };
            let (Ok(sb), Ok(bb)) = (std::fs::read(dir.join("phases_small").join(f)), std::fs::read(dir.join("images").join(format!("{stem}.dds"))))
            else {
                continue;
            };
            let (Ok(small), Ok(big)) = (crate::dds::decode(&sb), crate::dds::decode(&bb)) else { continue };
            let ours = phase_small(&big).unwrap();
            let ((d0, r0), (d1, r1)) = (phase_disc(&small), phase_disc(&ours));
            assert!((d0[2] - d1[2]).abs() < 1.2 && (d0[0] - d1[0]).abs() < 1.2 && (d0[1] - d1[1]).abs() < 1.2, "{f}: {d0:?} vs {d1:?}");
            rings_v.push(r0);
            rings_o.push(r1);
            // the grey of the inner disc
            let grey = |i: &Rgba| -> Vec<f64> {
                let mut v = Vec::new();
                for y in 17..41 {
                    for x in 18..42 {
                        let p = i.px(x, y);
                        v.push(0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]));
                    }
                }
                v
            };
            let (a, b) = (grey(&small), grey(&ours));
            let (ma, mb) = (a.iter().sum::<f64>() / a.len() as f64, b.iter().sum::<f64>() / b.len() as f64);
            let num: f64 = a.iter().zip(&b).map(|(x, y)| (x - ma) * (y - mb)).sum();
            let den = (a.iter().map(|x| (x - ma).powi(2)).sum::<f64>() * b.iter().map(|y| (y - mb).powi(2)).sum::<f64>()).sqrt();
            corr.push(if den > 0.0 { num / den } else { 0.0 });
            n += 1;
        }
        corr.sort_by(f64::total_cmp);
        let median = corr.get(corr.len() / 2).copied().unwrap_or(0.0);
        eprintln!("phase icons: {n} pairs, median correlation of the inner disc {median:.2}");
        assert!(n >= 60, "{n} pairs");
        // vanilla's edge varies from file to file (one even has a light rim), so the medians are compared
        let med = |v: &[[f64; 3]], k: usize| {
            let mut x: Vec<f64> = v.iter().map(|r| r[k]).collect();
            x.sort_by(f64::total_cmp);
            x[x.len() / 2]
        };
        for k in 0..3 {
            let (a, b) = (med(&rings_v, k), med(&rings_o, k));
            eprintln!("ring {k}: vanilla median {a:.3}, ours {b:.3}");
            assert!((a - b).abs() < 0.08, "ring {k}: {a} vs {b}");
        }
        // vanilla crops each icon by hand, so a fixed crop matches only moderately (see PHASE_SMALL_CROP)
        assert!(median > 0.22, "the icons show the same picture: {median}");
    }

    fn disc(w: u32, h: u32, cx: f64, cy: f64, r: f64, rgb: [u8; 3]) -> Rgba {
        let mut img = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let d = ((f64::from(x) + 0.5 - cx).powi(2) + (f64::from(y) + 0.5 - cy).powi(2)).sqrt();
                if d <= r {
                    img.set_px(x, y, [rgb[0], rgb[1], rgb[2], 255]);
                }
            }
        }
        img
    }

    #[test]
    fn the_small_emblem_is_a_grey_30x12_with_a_dark_outline() {
        let large = disc(76, 42, 38.0, 21.0, 16.0, [60, 110, 70]);
        let small = small_emblem(&large).unwrap();
        assert_eq!((small.width, small.height), (30, 12));
        let b = small.alpha_bbox(1).unwrap();
        assert!(b[3] - b[1] >= 10, "the subject fills the height: {b:?}");
        // grey: every visible pixel has r == g == b, opaque pixels are light, the outline is black
        let mut light = 0;
        for p in small.data.chunks_exact(4).filter(|p| p[3] > 0) {
            assert!(p[0] == p[1] && p[1] == p[2], "{p:?}");
            if p[3] == 255 {
                assert!(p[0] >= 100, "{p:?}");
                light += 1;
            }
        }
        assert!(light > 30);
        assert_eq!(small.px(b[0], 6)[0], 0, "the left edge is the outline");
        // deterministic
        assert_eq!(small_emblem(&large).unwrap().pixels_sha256(), small.pixels_sha256());
        assert!(small_emblem(&Rgba::new(76, 42)).is_err(), "an empty emblem has nothing to derive from");
    }

    #[test]
    fn the_text_icon_tints_frame_one_and_keeps_frame_two() {
        let mut strip = Rgba::filled(60, 12, [200, 200, 200, 255]);
        strip.set_px(40, 5, [10, 20, 30, 128]);
        let t = text_icon(&strip, 2).unwrap();
        assert_eq!(t.px(3, 3), [115, 147, 115, 255]);
        assert_eq!(t.px(40, 5), [10, 20, 30, 128]);
        assert_eq!(t.crop(30, 0, 30, 12).unwrap(), strip.crop(30, 0, 30, 12).unwrap());
    }

    #[test]
    fn the_miniature_fits_frame_one_edge_to_edge() {
        let mut strip = Rgba::new(200, 100);
        strip.paste(&disc(100, 100, 50.0, 50.0, 40.0, [200, 30, 30]), 0, 0);
        strip.paste(&disc(100, 100, 50.0, 50.0, 45.0, [255, 255, 255]), 100, 0);
        let m = miniature(&strip, 2).unwrap();
        assert_eq!((m.width, m.height), (32, 32));
        let b = m.alpha_bbox(16).unwrap();
        assert_eq!((b[2] - b[0], b[3] - b[1]), (32, 32));
        assert!(m.px(16, 16)[0] > 150 && m.px(16, 16)[1] < 80, "frame 1, not frame 2");
    }

    /// Vanilla's large landmark strips, made small by the derivation, against vanilla's own small strips of the same
    /// landmarks: the same orange (inside median within 15 per channel), the gradient from top to bottom, and about
    /// the same size in the frame.
    #[test]
    fn the_small_landmark_matches_vanillas_orange_pictograms() {
        use crate::recipes::raid::raid_stats;
        let g = std::env::var("HOI4_GAME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| "C:/Program Files (x86)/Steam/steamapps/common/Hearts of Iron IV".into());
        let mut stack = vec![g.join("gfx/interface/buildings")];
        let mut files = std::collections::BTreeMap::new();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Some(img) = std::fs::read(&p).ok().and_then(|b| crate::dds::decode(&b).ok()) {
                    files.insert(p.file_stem().unwrap().to_string_lossy().to_string(), img);
                }
            }
        }
        let pairs: Vec<(Rgba, Rgba)> = files
            .iter()
            .filter(|(n, i)| (i.width, i.height) == (128, 64) && !n.ends_with("_small"))
            .filter_map(|(n, large)| files.get(&format!("{n}_small")).filter(|s| (s.width, s.height) == (64, 32)).map(|s| (large.clone(), s.clone())))
            .collect();
        if pairs.len() < 10 {
            eprintln!("SKIPPED small landmark vs vanilla: Hearts of Iron IV not found (set HOI4_GAME_DIR)");
            return;
        }
        let first = |s: &Rgba| s.split_frames(2).unwrap().swap_remove(0);
        let van: Vec<Rgba> = pairs.iter().map(|(_, s)| first(s)).collect();
        let ours: Vec<Rgba> = pairs.iter().map(|(l, _)| first(&landmark_small(l, 2).unwrap())).collect();
        let halved: Vec<Rgba> = pairs.iter().map(|(l, _)| first(&half_size(l, 2).unwrap())).collect();
        let stats = |set: &[Rgba]| raid_stats(&set.iter().collect::<Vec<_>>());
        let (v, o, h) = (stats(&van), stats(&ours), stats(&halved));
        let span = |set: &[Rgba]| {
            let mut w: Vec<u32> = set.iter().filter_map(|i| i.alpha_bbox(128)).map(|b| b[2] - b[0]).collect();
            w.sort();
            w[w.len() / 2]
        };
        eprintln!(
            "small landmark, {} pairs, medians\n  vanilla {v:?} width {}\n  half size (before) {h:?} width {}\n  ours {o:?} width {}",
            pairs.len(),
            span(&van),
            span(&halved),
            span(&ours)
        );
        for c in 0..3 {
            assert!((o.core_rgb[c] - v.core_rgb[c]).abs() <= 15.0, "channel {c}: {o:?} vs {v:?}");
        }
        assert!(o.bands[0] > o.bands[4] + 20.0, "light at the top: {o:?}");
        for (a, b) in o.bands.iter().zip(v.bands) {
            assert!((a - b).abs() <= 20.0, "bands {:?} vs {:?}", o.bands, v.bands);
        }
        assert!((i64::from(span(&ours)) - i64::from(span(&van))).abs() <= 4);
    }

    #[test]
    fn half_size_halves_every_frame() {
        let strip = Rgba::filled(128, 64, [10, 20, 30, 255]);
        let h = half_size(&strip, 2).unwrap();
        assert_eq!((h.width, h.height), (64, 32));
        assert!(half_size(&Rgba::new(57, 84), 1).is_err());
    }
}
