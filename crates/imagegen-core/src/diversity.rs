//! Near-duplicate frames and compositions: a perceptual hash and its family threshold.
//!
//! The hash is a 128-bit difference hash (dHash) of the image composited over mid grey at its own size: 64 bits
//! compare horizontal neighbours of a 9x8 area-averaged grey thumbnail, 64 bits vertical neighbours of an 8x9 one.
//! It sees the outline of the frame and the placement of the large shapes, not colour or fine detail.
//!
//! What counts as near is the family's own: the threshold is the 1st percentile of the distances between distinct
//! files of the family's reference set (vanilla when it falls to vanilla, up to 256 files sampled evenly), so a warning
//! means two outputs are closer than 99 in 100 pairs of the family's own art. Measured on HOI4 1.19.5.0: focus 28,
//! national spirits 31, decisions 27, decision categories 15, report events 41, unit leader traits 22 and
//! technologies 24 bits. Vanilla's own recolours and copies sit at 0 to 6 bits.

use crate::pixels::Rgba;

/// Bits in the hash.
pub const HASH_BITS: u32 = 128;
/// The percentile of a family's own pairwise distances used as its threshold.
pub const THRESHOLD_PERCENTILE: f64 = 0.01;
/// Lowest threshold, whatever the family's spread: below it two images are near copies anyway.
pub const MIN_THRESHOLD: u32 = 6;
/// Threshold used when the family has no reference set to measure (the median of the measured families' thresholds
/// is 28; the fallback stays well below it so that only clear near-copies warn).
pub const FALLBACK_THRESHOLD: u32 = 16;

fn grey_over_mid(px: [u8; 4]) -> f64 {
    let a = f64::from(px[3]) / 255.0;
    let lum = 0.299 * f64::from(px[0]) + 0.587 * f64::from(px[1]) + 0.114 * f64::from(px[2]);
    lum * a + 128.0 * (1.0 - a)
}

/// Area-averaged grey thumbnail of `w` by `h`.
fn thumbnail(img: &Rgba, w: usize, h: usize) -> Vec<f64> {
    let (iw, ih) = (img.width as usize, img.height as usize);
    let mut sum = vec![0f64; w * h];
    let mut weight = vec![0f64; w * h];
    if iw == 0 || ih == 0 {
        return sum;
    }
    for y in 0..ih {
        let ty = y * h / ih;
        for x in 0..iw {
            let tx = x * w / iw;
            sum[ty * w + tx] += grey_over_mid(img.px(x as u32, y as u32));
            weight[ty * w + tx] += 1.0;
        }
    }
    sum.iter().zip(&weight).map(|(s, n)| if *n > 0.0 { s / n } else { 128.0 }).collect()
}

/// The 128-bit dHash of an image at its own size, composited over mid grey.
pub fn dhash(img: &Rgba) -> u128 {
    let mut bits: u128 = 0;
    let a = thumbnail(img, 9, 8);
    for y in 0..8 {
        for x in 0..8 {
            bits = (bits << 1) | u128::from(a[y * 9 + x] < a[y * 9 + x + 1]);
        }
    }
    let b = thumbnail(img, 8, 9);
    for y in 0..8 {
        for x in 0..8 {
            bits = (bits << 1) | u128::from(b[y * 8 + x] < b[(y + 1) * 8 + x]);
        }
    }
    bits
}

/// Bits that differ.
pub fn distance(a: u128, b: u128) -> u32 {
    (a ^ b).count_ones()
}

/// The family threshold from its own set: the [`THRESHOLD_PERCENTILE`] (nearest rank) of all pairwise distances,
/// at least [`MIN_THRESHOLD`]. `None` with fewer than three images.
pub fn threshold_from(hashes: &[u128]) -> Option<(u32, usize)> {
    if hashes.len() < 3 {
        return None;
    }
    let mut d: Vec<u32> = Vec::with_capacity(hashes.len() * (hashes.len() - 1) / 2);
    for i in 0..hashes.len() {
        for j in i + 1..hashes.len() {
            d.push(distance(hashes[i], hashes[j]));
        }
    }
    d.sort_unstable();
    let rank = ((d.len() as f64 * THRESHOLD_PERCENTILE).ceil() as usize).clamp(1, d.len()) - 1;
    Some((d[rank].max(MIN_THRESHOLD), d.len()))
}

/// Width and height a flag is compared at.
pub const VARIANT_SIZE: (usize, usize) = (82, 52);
/// Share of the base's edge pixels that reappear in a variant at or above which the variant keeps the base design.
pub const VARIANT_CONTAINMENT: f64 = 0.70;
/// A base with fewer edge pixels (a plain field) cannot be judged.
pub const VARIANT_MIN_EDGES: usize = 30;
/// Summed RGB step between neighbouring pixels that counts as an edge.
const EDGE_STEP: i32 = 48;

fn rgb_thumbnail(img: &Rgba, w: usize, h: usize) -> Vec<[i32; 3]> {
    let (iw, ih) = (img.width as usize, img.height as usize);
    let mut sum = vec![[0f64; 3]; w * h];
    let mut n = vec![0f64; w * h];
    if iw == 0 || ih == 0 {
        return vec![[128; 3]; w * h];
    }
    for y in 0..ih {
        for x in 0..iw {
            let i = (y * h / ih) * w + x * w / iw;
            let p = img.px(x as u32, y as u32);
            let a = f64::from(p[3]) / 255.0;
            for c in 0..3 {
                sum[i][c] += f64::from(p[c]) * a + 128.0 * (1.0 - a);
            }
            n[i] += 1.0;
        }
    }
    sum.iter().zip(&n).map(|(s, n)| if *n > 0.0 { [(s[0] / n) as i32, (s[1] / n) as i32, (s[2] / n) as i32] } else { [128; 3] }).collect()
}

fn edge_map(img: &Rgba) -> Vec<bool> {
    let (w, h) = VARIANT_SIZE;
    let t = rgb_thumbnail(img, w, h);
    let step = |a: [i32; 3], b: [i32; 3]| (0..3).map(|c| (a[c] - b[c]).abs()).sum::<i32>() >= EDGE_STEP;
    let mut e = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            e[i] = (x + 1 < w && step(t[i], t[i + 1])) || (y + 1 < h && step(t[i], t[i + w]));
        }
    }
    e
}

fn dilate(e: &[bool]) -> Vec<bool> {
    let (w, h) = VARIANT_SIZE;
    let mut o = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if e[y * w + x] {
                for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                    for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                        o[yy * w + xx] = true;
                    }
                }
            }
        }
    }
    o
}

/// How much of the base flag's edge structure survives in a variant, from 0 to 1 (the better of the variant and
/// its mirror image), compared at 82x52. A recolour keeps every edge and a border, band, crown or dye laid over the
/// base keeps the base's own, so both score near 1. `None` when the base has too few edges to judge.
pub fn variant_containment(base: &Rgba, variant: &Rgba) -> Option<f64> {
    let b = edge_map(base);
    let count = b.iter().filter(|x| **x).count();
    if count < VARIANT_MIN_EDGES {
        return None;
    }
    let (w, h) = VARIANT_SIZE;
    let v = edge_map(variant);
    let mirrored: Vec<bool> = (0..w * h).map(|i| v[(i / w) * w + (w - 1 - i % w)]).collect();
    let best = [v, mirrored]
        .iter()
        .map(|m| {
            let near = dilate(m);
            b.iter().zip(&near).filter(|(a, n)| **a && **n).count() as f64 / count as f64
        })
        .fold(0.0, f64::max);
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plaque(cx: f64, r: f64, shade: u8) -> Rgba {
        let mut img = Rgba::new(100, 88);
        for y in 0..88u32 {
            for x in 0..100u32 {
                let d = ((f64::from(x) - cx).powi(2) + (f64::from(y) - 44.0).powi(2)).sqrt();
                if d < r {
                    let v = if d > r - 6.0 { 200 } else { shade.wrapping_add((x % 7) as u8) };
                    img.set_px(x, y, [v, v, v, 255]);
                }
            }
        }
        img
    }

    #[test]
    fn the_hash_sees_frame_and_layout_not_small_changes() {
        let a = dhash(&plaque(50.0, 38.0, 60));
        let b = dhash(&plaque(50.0, 38.0, 70));
        let c = dhash(&plaque(30.0, 22.0, 60));
        assert!(distance(a, b) <= 6, "same frame and layout: {}", distance(a, b));
        assert!(distance(a, c) > 20, "another layout: {}", distance(a, c));
        assert_eq!(distance(a, a), 0);
        // the hash ignores the size the image is stored at
        let mut big = Rgba::new(200, 176);
        let small = plaque(50.0, 38.0, 60);
        for y in 0..176 {
            for x in 0..200 {
                big.set_px(x, y, small.px(x / 2, y / 2));
            }
        }
        assert!(distance(a, dhash(&big)) <= 4);
    }

    #[test]
    fn the_threshold_is_a_low_percentile_of_the_set() {
        assert_eq!(threshold_from(&[0, 1]), None);
        let set: Vec<u128> = (0..40u32).map(|i| ((1u128 << (i % 128)) * 0xFFFF_FFFF) | u128::from(i)).collect();
        let (t, pairs) = threshold_from(&set).unwrap();
        assert_eq!(pairs, 40 * 39 / 2);
        assert!(t >= MIN_THRESHOLD);
        let identical = vec![7u128; 5];
        assert_eq!(threshold_from(&identical).unwrap().0, MIN_THRESHOLD);
    }

    fn flag(f: impl Fn(u32, u32) -> [u8; 3]) -> Rgba {
        let mut img = Rgba::new(82, 52);
        for y in 0..52 {
            for x in 0..82 {
                let c = f(x, y);
                img.set_px(x, y, [c[0], c[1], c[2], 255]);
            }
        }
        img
    }

    fn base_flag(x: u32, y: u32) -> [u8; 3] {
        if (30..52).contains(&x) && (10..42).contains(&y) {
            [20, 20, 120]
        } else if (x / 9 + y / 9) % 2 == 1 {
            [240, 220, 40]
        } else {
            [200, 30, 30]
        }
    }

    #[test]
    fn a_recolour_or_an_added_border_keeps_the_base_and_a_new_design_does_not() {
        let base = flag(base_flag);
        let recolour = flag(|x, y| {
            let c = base_flag(x, y);
            [c[2], c[0], c[1]]
        });
        let border = flag(|x, y| if x < 4 || y < 4 || x > 77 || y > 47 { [0, 0, 0] } else { base_flag(x, y) });
        let mirror = flag(|x, y| base_flag(81 - x, y));
        let other = flag(|x, y| {
            if (x + y) % 40 < 12 && y > 5 {
                [250, 250, 250]
            } else if x < 20 {
                [10, 60, 30]
            } else {
                [180, 140, 20]
            }
        });
        for (name, v) in [("recolour", &recolour), ("border", &border), ("mirror", &mirror)] {
            assert!(variant_containment(&base, v).unwrap() >= VARIANT_CONTAINMENT, "{name}");
        }
        assert!(variant_containment(&base, &other).unwrap() < VARIANT_CONTAINMENT);
        assert_eq!(variant_containment(&flag(|_, _| [9, 9, 9]), &other), None);
    }
}
