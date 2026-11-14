//! Style descriptors, deterministic k-medoids selection, style statistics and the drift model.
//!
//! A descriptor is a small vector of measured image properties: CIE Lab mean and spread, saturation, Sobel
//! edge density, alpha coverage, dark-outline presence and a coarse hue histogram. Everything is a pure function
//! of pixels, so the same images always give the same descriptor, the same medoids in the same order and the same
//! drift verdict. Nothing here judges quality; it measures how a family's existing art looks so a new asset can be
//! pointed at it and compared with it.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::family::Family;
use crate::pixels::Rgba;
use crate::resize::resize_lanczos3;

/// Number of descriptor dimensions.
pub const DIMS: usize = 16;

/// Names of the dimensions, in vector order.
pub const DIM_NAMES: [&str; DIMS] = [
    "lab_l_mean",
    "lab_a_mean",
    "lab_b_mean",
    "lab_l_std",
    "lab_a_std",
    "lab_b_std",
    "saturation_mean",
    "edge_density",
    "alpha_coverage",
    "outline_presence",
    "hue_red",
    "hue_yellow",
    "hue_green",
    "hue_cyan",
    "hue_blue",
    "hue_magenta",
];

/// Smallest spread a dimension is divided by when standardising. It stops identical or near-identical images
/// from turning a rounding difference into an enormous z-score.
const SD_FLOOR: [f64; DIMS] = [3.0, 3.0, 3.0, 2.0, 2.0, 2.0, 0.03, 0.02, 0.03, 0.03, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02];

/// Sobel magnitude (Lab lightness units per pixel) above which a pixel counts as an edge. Measured on vanilla: at 6
/// almost every pixel of dark painted art counted (mean density 0.8 for focus icons), which left no spread to compare.
pub const EDGE_THRESHOLD: f64 = 12.0;

/// Longest side measured. Larger images are shrunk first so edge density does not depend on the file's size.
pub const MAX_MEASURED_SIDE: u32 = 160;

/// Most images used for the statistics.
pub const STATS_IMAGES: usize = 24;

/// Lowest drift threshold, whatever the leave-one-out distances say.
pub const MIN_DRIFT_THRESHOLD: f64 = 1.75;

/// A point of the descriptor space.
pub type Vector = [f64; DIMS];

/// One image, measured.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The descriptor vector.
    pub v: Vector,
    /// Spread of saturation over visible pixels.
    pub sat_std: f64,
    /// Mean HSV value over visible pixels.
    pub val_mean: f64,
    /// Spread of HSV value over visible pixels.
    pub val_std: f64,
    /// Smallest margin to the visible box, in measured pixels.
    pub margin_px: f64,
    /// Larger of the visible box's width and height as a share of the image.
    pub span: f64,
    /// Colour buckets: (bucket, pixel count, summed r, g, b).
    pub palette: Vec<(u16, u32, [u64; 3])>,
    /// Pixel count behind the palette.
    pub palette_pixels: u32,
    /// The image's main hue in degrees: the saturation-weighted circular mean of the hues of its visible pixels with
    /// a value of 0.15 or more. `None` for a grey image (a weighted mean saturation under [`TINTED_MIN`]).
    pub main_hue: Option<f64>,
}

/// Lowest strength of an image's main hue (the length of the saturation-weighted mean hue vector over its pixels)
/// for it to count as tinted. A grey or black-and-white image has no main hue.
pub const TINTED_MIN: f64 = 0.03;

/// Hue spread from which a set counts as varied: its images do not share one colour, so a style paragraph does not
/// name one. Measured on HOI4 1.19.4.1 with [`hue_spread`] over up to 150 files a folder, every nth file in name
/// order: generic focus icons 0.05 (nearly all warm), national spirits 0.25, unit leader traits 0.47 and technology
/// pictures 0.58. On a random sample of 150: achievements 0.07, decisions 0.16, state modifiers 0.20 and MIO policies
/// 0.64.
pub const VARIED_HUE_SPREAD: f64 = 0.25;

/// The spread of the main hues of a set: 1 minus the length of the mean unit vector of the tinted images' main
/// hues. 0 when every tinted image has the same hue, near 1 when their hues are spread around the colour wheel, 0
/// when fewer than two images are tinted.
pub fn hue_spread(hues: &[f64]) -> f64 {
    if hues.len() < 2 {
        return 0.0;
    }
    let (sx, sy) = hues.iter().fold((0.0, 0.0), |(x, y), h| (x + h.to_radians().cos(), y + h.to_radians().sin()));
    1.0 - sx.hypot(sy) / hues.len() as f64
}

fn srgb_lut() -> [f64; 256] {
    let mut lut = [0f64; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let c = i as f64 / 255.0;
        *slot = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    }
    lut
}

fn to_lab(r: u8, g: u8, b: u8, lut: &[f64; 256]) -> [f64; 3] {
    let (r, g, b) = (lut[usize::from(r)], lut[usize::from(g)], lut[usize::from(b)]);
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let f = |t: f64| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Hue in degrees (0..360), saturation and value of an sRGB colour.
pub fn hsv(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let (r, g, b) = (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max <= 0.0 { 0.0 } else { d / max };
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, max)
}

/// Measure an image. `None` when it has no visible pixel (alpha of 16 or more).
pub fn analyse(src: &Rgba) -> Option<Analysis> {
    let longest = src.width.max(src.height);
    let shrunk;
    let img = if longest > MAX_MEASURED_SIDE {
        let scale = f64::from(MAX_MEASURED_SIDE) / f64::from(longest);
        let nw = ((f64::from(src.width) * scale).round() as u32).max(1);
        let nh = ((f64::from(src.height) * scale).round() as u32).max(1);
        shrunk = resize_lanczos3(src, nw, nh).ok()?;
        &shrunk
    } else {
        src
    };
    let (w, h) = (img.width as usize, img.height as usize);
    let n = w * h;
    let lut = srgb_lut();
    let mut vis = vec![false; n];
    let mut lum = vec![0f64; n];
    let (mut count, mut sum_l, mut sum_a, mut sum_b) = (0u64, 0f64, 0f64, 0f64);
    let (mut sq_l, mut sq_a, mut sq_b) = (0f64, 0f64, 0f64);
    let (mut sum_s, mut sq_s, mut sum_v, mut sq_v) = (0f64, 0f64, 0f64, 0f64);
    let mut hue = [0u64; 6];
    let (mut hue_x, mut hue_y, mut hue_n) = (0f64, 0f64, 0u64);
    let mut buckets: BTreeMap<u16, (u32, [u64; 3])> = BTreeMap::new();
    let mut palette_pixels = 0u32;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let p = &img.data[i * 4..i * 4 + 4];
            if p[3] < 16 {
                continue;
            }
            vis[i] = true;
            count += 1;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
            let lab = to_lab(p[0], p[1], p[2], &lut);
            lum[i] = lab[0];
            sum_l += lab[0];
            sum_a += lab[1];
            sum_b += lab[2];
            sq_l += lab[0] * lab[0];
            sq_a += lab[1] * lab[1];
            sq_b += lab[2] * lab[2];
            let (hd, s, v) = hsv(p[0], p[1], p[2]);
            sum_s += s;
            sq_s += s * s;
            sum_v += v;
            sq_v += v * v;
            if s >= 0.15 && v >= 0.12 {
                hue[(((hd + 30.0) / 60.0).floor() as usize) % 6] += 1;
            }
            if p[3] >= 200 && v >= 0.15 {
                hue_x += hd.to_radians().cos() * s;
                hue_y += hd.to_radians().sin() * s;
                hue_n += 1;
            }
            if p[3] >= 128 {
                let key = (u16::from(p[0] >> 5) << 6) | (u16::from(p[1] >> 5) << 3) | u16::from(p[2] >> 5);
                let e = buckets.entry(key).or_insert((0, [0; 3]));
                e.0 += 1;
                e.1[0] += u64::from(p[0]);
                e.1[1] += u64::from(p[1]);
                e.1[2] += u64::from(p[2]);
                palette_pixels += 1;
            }
        }
    }
    if count == 0 {
        return None;
    }
    let c = count as f64;
    let sd = |sq: f64, mean: f64| (sq / c - mean * mean).max(0.0).sqrt();
    let (ml, ma, mb) = (sum_l / c, sum_a / c, sum_b / c);
    let (ms, mv) = (sum_s / c, sum_v / c);

    // edge density: Sobel on Lab lightness over interior visible pixels
    let (mut interior, mut edgy) = (0u64, 0u64);
    if w >= 3 && h >= 3 {
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let idx = |dx: usize, dy: usize| (y + dy - 1) * w + (x + dx - 1);
                if !(0..3).all(|dy| (0..3).all(|dx| vis[idx(dx, dy)])) {
                    continue;
                }
                let l = |dx: usize, dy: usize| lum[idx(dx, dy)];
                let gx = (l(2, 0) + 2.0 * l(2, 1) + l(2, 2)) - (l(0, 0) + 2.0 * l(0, 1) + l(0, 2));
                let gy = (l(0, 2) + 2.0 * l(1, 2) + l(2, 2)) - (l(0, 0) + 2.0 * l(1, 0) + l(2, 0));
                interior += 1;
                if gx.hypot(gy) / 4.0 > EDGE_THRESHOLD {
                    edgy += 1;
                }
            }
        }
    }
    let edge_density = if interior < 8 { 0.0 } else { edgy as f64 / interior as f64 };

    // outline presence: the share of rim pixels darker than the subject's mean lightness
    let has_hole = (count as usize) < n;
    let (mut rim, mut dark) = (0u64, 0u64);
    let dark_limit = ml - 12.0;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let on_rim = if has_hole {
                img.data[i * 4 + 3] >= 128
                    && [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)]
                        .into_iter()
                        .flatten()
                        .any(|j| img.data[j * 4 + 3] < 16)
            } else {
                x == 0 || y == 0 || x + 1 == w || y + 1 == h
            };
            if on_rim && vis[i] {
                rim += 1;
                if lum[i] < dark_limit {
                    dark += 1;
                }
            }
        }
    }
    let outline = if rim == 0 { 0.0 } else { dark as f64 / rim as f64 };

    let mut v = [0f64; DIMS];
    v[0] = ml;
    v[1] = ma;
    v[2] = mb;
    v[3] = sd(sq_l, ml);
    v[4] = sd(sq_a, ma);
    v[5] = sd(sq_b, mb);
    v[6] = ms;
    v[7] = edge_density;
    v[8] = c / n as f64;
    v[9] = outline;
    for (k, hv) in hue.iter().enumerate() {
        v[10 + k] = *hv as f64 / c;
    }
    let span = (((x1 - x0) as f64) / w as f64).max(((y1 - y0) as f64) / h as f64);
    let margin = x0.min(y0).min(w - x1).min(h - y1) as f64;
    Some(Analysis {
        v,
        sat_std: sd(sq_s, ms),
        val_mean: mv,
        val_std: sd(sq_v, mv),
        margin_px: margin,
        span,
        palette: buckets.into_iter().map(|(k, (n, s))| (k, n, s)).collect(),
        palette_pixels,
        main_hue: (hue_n > 0 && hue_x.hypot(hue_y) / hue_n as f64 >= TINTED_MIN).then(|| hue_y.atan2(hue_x).to_degrees().rem_euclid(360.0)),
    })
}

/// Per-dimension mean and spread of a set of descriptors.
#[derive(Debug, Clone, PartialEq)]
pub struct Standardiser {
    /// Means.
    pub mean: Vector,
    /// Spreads, never below the dimension's floor.
    pub sd: Vector,
}

impl Standardiser {
    /// Fit to rows (population spread, floored).
    pub fn fit(rows: &[Vector]) -> Standardiser {
        let n = rows.len().max(1) as f64;
        let mut mean = [0f64; DIMS];
        for r in rows {
            for d in 0..DIMS {
                mean[d] += r[d];
            }
        }
        for m in &mut mean {
            *m /= n;
        }
        let mut sd = [0f64; DIMS];
        for r in rows {
            for d in 0..DIMS {
                sd[d] += (r[d] - mean[d]).powi(2);
            }
        }
        for d in 0..DIMS {
            sd[d] = (sd[d] / n).sqrt().max(SD_FLOOR[d]);
        }
        Standardiser { mean, sd }
    }

    /// z-scores of a vector.
    pub fn z(&self, v: &Vector) -> Vector {
        let mut out = [0f64; DIMS];
        for d in 0..DIMS {
            out[d] = (v[d] - self.mean[d]) / self.sd[d];
        }
        out
    }
}

fn dist(a: &Vector, b: &Vector) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt()
}

/// Result of k-medoids.
#[derive(Debug, Clone, PartialEq)]
pub struct Medoids {
    /// Indices into the input, ordered by cluster size (largest first), then index.
    pub medoids: Vec<usize>,
    /// Cluster sizes, aligned with `medoids`.
    pub sizes: Vec<usize>,
}

/// Deterministic k-medoids (PAM): the greedy BUILD phase, then best-improvement SWAP until nothing improves.
/// There is no random start. Ties go to the lower index, so the caller fixes the order of `points` (the selector
/// orders them by content hash) and the answer depends on the images alone. Points are standardised first.
#[allow(clippy::needless_range_loop)]
pub fn kmedoids(points: &[Vector], k: usize) -> Medoids {
    let n = points.len();
    let k = k.min(n);
    if k == 0 {
        return Medoids { medoids: vec![], sizes: vec![] };
    }
    let st = Standardiser::fit(points);
    let z: Vec<Vector> = points.iter().map(|p| st.z(p)).collect();
    let d: Vec<Vec<f64>> = (0..n).map(|i| (0..n).map(|j| dist(&z[i], &z[j])).collect()).collect();

    // BUILD
    let first = (0..n).map(|i| (i, d[i].iter().sum::<f64>())).fold((0usize, f64::INFINITY), |best, c| if c.1 < best.1 - 1e-12 { c } else { best }).0;
    let mut med = vec![first];
    let mut nearest: Vec<f64> = (0..n).map(|j| d[first][j]).collect();
    while med.len() < k {
        let mut best = (usize::MAX, -1.0f64);
        for c in 0..n {
            if med.contains(&c) {
                continue;
            }
            let gain: f64 = (0..n).map(|j| (nearest[j] - d[c][j]).max(0.0)).sum();
            if gain > best.1 + 1e-12 {
                best = (c, gain);
            }
        }
        if best.0 == usize::MAX {
            break;
        }
        med.push(best.0);
        for j in 0..n {
            nearest[j] = nearest[j].min(d[best.0][j]);
        }
    }

    // SWAP
    for _ in 0..50 {
        let mut near1 = vec![(f64::INFINITY, 0usize); n];
        let mut near2 = vec![f64::INFINITY; n];
        for j in 0..n {
            for (pos, &m) in med.iter().enumerate() {
                let dj = d[m][j];
                if dj < near1[j].0 {
                    near2[j] = near1[j].0;
                    near1[j] = (dj, pos);
                } else if dj < near2[j] {
                    near2[j] = dj;
                }
            }
        }
        let current: f64 = near1.iter().map(|x| x.0).sum();
        let mut best = (0usize, 0usize, current);
        for pos in 0..med.len() {
            for h in 0..n {
                if med.contains(&h) {
                    continue;
                }
                let cost: f64 = (0..n)
                    .map(|j| {
                        let other = if near1[j].1 == pos { near2[j] } else { near1[j].0 };
                        d[h][j].min(other)
                    })
                    .sum();
                if cost < best.2 - 1e-9 {
                    best = (pos, h, cost);
                }
            }
        }
        if best.2 >= current - 1e-9 {
            break;
        }
        med[best.0] = best.1;
    }

    let mut sizes = vec![0usize; med.len()];
    for j in 0..n {
        let mut b = (0usize, f64::INFINITY);
        for (pos, &m) in med.iter().enumerate() {
            if d[m][j] < b.1 - 1e-12 {
                b = (pos, d[m][j]);
            }
        }
        sizes[b.0] += 1;
    }
    let mut order: Vec<usize> = (0..med.len()).collect();
    order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]).then(med[a].cmp(&med[b])));
    Medoids { medoids: order.iter().map(|&i| med[i]).collect(), sizes: order.iter().map(|&i| sizes[i]).collect() }
}

/// A palette entry of the style statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaletteColor {
    /// `#rrggbb` of the mean colour of the bucket.
    pub hex: String,
    /// Plain-word name.
    pub name: String,
    /// Share of the statistics images' opaque pixels, averaged per image.
    pub share: f64,
}

/// A mean and a spread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MeanStd {
    /// Mean over the images of the per-image mean.
    pub mean: f64,
    /// Mean over the images of the per-image spread.
    pub std: f64,
}

/// Measured style of a set of images.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleStats {
    /// Images measured.
    pub images: usize,
    /// Most common colours.
    pub palette: Vec<PaletteColor>,
    /// Saturation (HSV, 0 to 1).
    pub saturation: MeanStd,
    /// Value (HSV brightness, 0 to 1).
    pub value: MeanStd,
    /// Share of interior pixels on a Sobel edge.
    pub edge_density: f64,
    /// Share of rim pixels darker than the subject.
    pub outline_presence: f64,
    /// Share of pixels that are visible.
    pub alpha_coverage: f64,
    /// Mean smallest margin to the visible box, in pixels of the measured image.
    pub margin_px: f64,
    /// Mean share of the frame the visible box spans (larger side).
    pub subject_span: f64,
    /// [`hue_spread`] of the images' main hues: from [`VARIED_HUE_SPREAD`] on, the images do not share one colour.
    #[serde(default)]
    pub hue_spread: f64,
}

/// Plain-word name of a colour.
pub fn colour_name(r: u8, g: u8, b: u8) -> String {
    let (h, s, v) = hsv(r, g, b);
    if v < 0.16 {
        return "black".into();
    }
    if s < 0.13 {
        return match v {
            v if v < 0.38 => "dark grey",
            v if v < 0.68 => "grey",
            v if v < 0.88 => "light grey",
            _ => "white",
        }
        .into();
    }
    let base = match h {
        h if !(15.0..345.0).contains(&h) => "red",
        h if h < 42.0 => {
            if v < 0.6 {
                "brown"
            } else {
                "orange"
            }
        }
        h if h < 66.0 => {
            if v < 0.55 {
                "ochre"
            } else {
                "yellow"
            }
        }
        h if h < 90.0 => {
            if v < 0.62 {
                "olive"
            } else {
                "lime"
            }
        }
        h if h < 160.0 => "green",
        h if h < 195.0 => "teal",
        h if h < 255.0 => "blue",
        h if h < 290.0 => "purple",
        h if h < 330.0 => "magenta",
        _ => "crimson",
    };
    let shade = if v < 0.33 && !matches!(base, "brown" | "olive" | "ochre") {
        "dark "
    } else if v > 0.82 && s < 0.45 {
        "pale "
    } else {
        ""
    };
    format!("{shade}{base}")
}

/// Aggregate the measurements of the statistics images.
pub fn aggregate(items: &[&Analysis]) -> StyleStats {
    let n = items.len().max(1) as f64;
    let mean = |f: &dyn Fn(&Analysis) -> f64| items.iter().map(|a| f(a)).sum::<f64>() / n;
    let mut share: BTreeMap<u16, (f64, f64, [f64; 3])> = BTreeMap::new();
    for a in items {
        let total = f64::from(a.palette_pixels.max(1));
        for (key, count, rgb) in &a.palette {
            let e = share.entry(*key).or_insert((0.0, 0.0, [0.0; 3]));
            e.0 += f64::from(*count) / total;
            e.1 += f64::from(*count);
            for (acc, v) in e.2.iter_mut().zip(rgb) {
                *acc += *v as f64;
            }
        }
    }
    let mut ranked: Vec<(u16, f64, [u8; 3])> = share
        .into_iter()
        .map(|(k, (s, cnt, rgb))| (k, s / n, [(rgb[0] / cnt).round() as u8, (rgb[1] / cnt).round() as u8, (rgb[2] / cnt).round() as u8]))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    let round3 = |x: f64| (x * 1000.0).round() / 1000.0;
    StyleStats {
        images: items.len(),
        palette: ranked
            .into_iter()
            .take(6)
            .map(|(_, s, c)| PaletteColor {
                hex: format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
                name: colour_name(c[0], c[1], c[2]),
                share: round3(s),
            })
            .collect(),
        saturation: MeanStd { mean: round3(mean(&|a| a.v[6])), std: round3(mean(&|a| a.sat_std)) },
        value: MeanStd { mean: round3(mean(&|a| a.val_mean)), std: round3(mean(&|a| a.val_std)) },
        edge_density: round3(mean(&|a| a.v[7])),
        outline_presence: round3(mean(&|a| a.v[9])),
        alpha_coverage: round3(mean(&|a| a.v[8])),
        margin_px: round3(mean(&|a| a.margin_px)),
        subject_span: round3(mean(&|a| a.span)),
        hue_spread: round3(hue_spread(&items.iter().filter_map(|a| a.main_hue).collect::<Vec<_>>())),
    }
}

/// A short style paragraph derived only from the measured values. `subject` says what was measured
/// (for example `the mod's existing national focus icon art`).
pub fn style_paragraph(subject: &str, s: &StyleStats) -> String {
    style_paragraph_for(subject, s, None)
}

/// Words a line-art paragraph never uses: they describe shaded bodies, which a line drawing has none of.
pub const SHADING_WORDS: &[&str] = &["shad", "surface", "texture", "fill"];

/// [`style_paragraph`] for a family whose processing fixes part of its look, so the paragraph never contradicts the
/// family's form rule:
///
/// * `special_project_icon` is redrawn as line art: the paragraph speaks of line weight, line colour and empty
///   interiors, and never of shading or surfaces (no word of [`SHADING_WORDS`]).
/// * `raid` is recoloured in vanilla's gold gradient with a black outline, and `doctrine` gets black units with a pale
///   outline and glow: the paragraph says so instead of describing the references' outline and surfaces, which the
///   processing replaces.
pub fn style_paragraph_for(subject: &str, s: &StyleStats, family: Option<Family>) -> String {
    let covered = format!("about {:.0}% of the canvas covered by the subject", s.alpha_coverage * 100.0);
    match family {
        Some(Family::SpecialProjectIcon) => {
            let colour = s.palette.first().map_or_else(|| "white".to_string(), |c| c.name.clone());
            return format!(
                "Match {subject}: line art only, thin {colour} lines about one pixel wide and partly transparent, so they read pale; the outline and a few inner lines carry the drawing and every interior stays empty; about {:.0}% of the canvas inked (measured on {} images).",
                s.alpha_coverage * 100.0,
                s.images
            );
        }
        Some(Family::Raid) => {
            return format!(
                "Match {subject}: one bold pictogram with clear light and dark details; the tool gives it vanilla's gold gradient, pale gold at the top to brown at the bottom, and the black outline on its edge; {covered} (measured on {} images).",
                s.images
            );
        }
        Some(Family::Doctrine) => {
            return format!(
                "Match {subject}: flat unit silhouettes grouped into one emblem with bright orange or yellow accents; the tool fills the units black and adds the pale outline and glow; {covered} (measured on {} images).",
                s.images
            );
        }
        _ => {}
    }
    let sat = match s.saturation.mean {
        x if x < 0.18 => "muted, low-saturation",
        x if x < 0.38 => "moderately muted",
        x if x < 0.62 => "moderately saturated",
        _ => "vivid, highly saturated",
    };
    let tone = match s.value.mean {
        x if x < 0.35 => "dark",
        x if x > 0.72 => "bright",
        _ => "mid-tone",
    };
    let mut names: Vec<String> = Vec::new();
    for c in &s.palette {
        if !names.contains(&c.name) {
            names.push(c.name.clone());
        }
        if names.len() == 3 {
            break;
        }
    }
    let listed = match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        2 => format!("{} and {}", names[0], names[1]),
        _ => format!("{}, {} and {}", names[0], names[1], names[2]),
    };
    // a varied set has no one colour: naming its averaged colours as "dominant" pulls every new image to them
    let dominated = if listed.is_empty() {
        String::new()
    } else if s.hue_spread >= VARIED_HUE_SPREAD {
        format!(
            " whose main colour changes from image to image with the subject (the most common colours are {listed}, but no one colour is the rule)"
        )
    } else {
        format!(" dominated by {listed}")
    };
    let mut parts = vec![format!("{sat}, {tone} palette{dominated}")];
    if s.alpha_coverage < 0.98 {
        parts.push(match s.outline_presence {
            x if x >= 0.6 => "strong dark outline".to_string(),
            x if x >= 0.3 => "partial dark outline".to_string(),
            _ => "little or no dark outline".to_string(),
        });
    }
    parts.push(
        match s.edge_density {
            x if x < 0.12 => "smooth, softly shaded surfaces",
            x if x > 0.35 => "crisp, detailed texture",
            _ => "moderate surface detail",
        }
        .to_string(),
    );
    if s.alpha_coverage < 0.98 {
        parts.push(covered);
    }
    format!("Match {subject}: {} (measured on {} images).", parts.join("; "), s.images)
}

/// Drift model: how far an image may be from a set before it counts as off-style.
#[derive(Debug, Clone, PartialEq)]
pub struct DriftModel {
    /// Standardiser fitted to the whole set.
    pub std: Standardiser,
    /// Images in the set.
    pub images: usize,
    /// 95th percentile (nearest rank) of the leave-one-out RMS z-scores of the set's own images.
    pub p95_leave_one_out: f64,
    /// Threshold used: the larger of that and [`MIN_DRIFT_THRESHOLD`].
    pub threshold: f64,
}

/// The verdict for one image.
#[derive(Debug, Clone, PartialEq)]
pub struct Drift {
    /// RMS z-score over all dimensions.
    pub z_rms: f64,
    /// Threshold.
    pub threshold: f64,
    /// True above the threshold.
    pub drifted: bool,
    /// The three dimensions furthest from the set, with their z-scores.
    pub worst: Vec<(&'static str, f64)>,
}

fn rms(z: &Vector) -> f64 {
    (z.iter().map(|x| x * x).sum::<f64>() / DIMS as f64).sqrt()
}

impl DriftModel {
    /// Fit to the descriptors of a set (at least 3).
    pub fn fit(rows: &[Vector]) -> DriftModel {
        let n = rows.len();
        let std = Standardiser::fit(rows);
        let mut loo: Vec<f64> = Vec::with_capacity(n);
        if n >= 3 {
            let nf = n as f64;
            let mut sum = [0f64; DIMS];
            let mut sq = [0f64; DIMS];
            for r in rows {
                for d in 0..DIMS {
                    sum[d] += r[d];
                    sq[d] += r[d] * r[d];
                }
            }
            for r in rows {
                let mut z = [0f64; DIMS];
                for d in 0..DIMS {
                    let m = (sum[d] - r[d]) / (nf - 1.0);
                    let var = ((sq[d] - r[d] * r[d]) / (nf - 1.0) - m * m).max(0.0);
                    z[d] = (r[d] - m) / var.sqrt().max(SD_FLOOR[d]);
                }
                loo.push(rms(&z));
            }
        }
        loo.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p95 = if loo.is_empty() { 0.0 } else { loo[((loo.len() as f64 * 0.95).ceil() as usize).clamp(1, loo.len()) - 1] };
        DriftModel { std, images: n, p95_leave_one_out: p95, threshold: p95.max(MIN_DRIFT_THRESHOLD) }
    }

    /// Compare an image with the set.
    pub fn check(&self, v: &Vector) -> Drift {
        let z = self.std.z(v);
        let z_rms = rms(&z);
        let mut ranked: Vec<(&'static str, f64)> = DIM_NAMES.iter().copied().zip(z.iter().copied()).collect();
        ranked.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(3);
        Drift { z_rms, threshold: self.threshold, drifted: z_rms > self.threshold, worst: ranked }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, px: [u8; 4]) -> Rgba {
        Rgba::filled(w, h, px)
    }

    fn icon(rgb: [u8; 3], outline: bool) -> Rgba {
        let mut img = Rgba::new(40, 40);
        for y in 6..34 {
            for x in 6..34 {
                let rim = x == 6 || y == 6 || x == 33 || y == 33;
                let c = if rim && outline { [10, 10, 10] } else { rgb };
                img.set_px(x, y, [c[0], c[1], c[2], 255]);
            }
        }
        img
    }

    #[test]
    fn descriptor_measures_colour_alpha_and_outline() {
        let red = analyse(&icon([200, 30, 30], true)).unwrap();
        assert!(red.v[6] > 0.7, "saturation {}", red.v[6]);
        assert!((red.v[8] - 28.0 * 28.0 / 1600.0).abs() < 1e-9, "alpha coverage {}", red.v[8]);
        assert!(red.v[9] > 0.9, "outline {}", red.v[9]);
        assert!(red.v[10] > 0.8, "red hue share {}", red.v[10]);
        assert_eq!(red.margin_px, 6.0);
        let grey = analyse(&icon([120, 120, 120], false)).unwrap();
        assert!(grey.v[6] < 0.01 && grey.v[9] == 0.0 && grey.v[10] == 0.0);
        assert!(analyse(&Rgba::new(8, 8)).is_none());
        let opaque = analyse(&flat(30, 20, [50, 90, 160, 255])).unwrap();
        assert_eq!(opaque.v[8], 1.0);
    }

    #[test]
    fn colours_have_plain_names() {
        assert_eq!(colour_name(200, 30, 30), "red");
        assert_eq!(colour_name(110, 110, 110), "grey");
        assert_eq!(colour_name(5, 5, 5), "black");
        assert_eq!(colour_name(110, 120, 60), "olive");
        assert_eq!(colour_name(30, 60, 170), "blue");
    }

    fn rows(shift: f64) -> Vec<Vector> {
        (0..12)
            .map(|i| {
                let mut v = [0f64; DIMS];
                for (d, slot) in v.iter_mut().enumerate() {
                    *slot = shift + ((i * 7 + d * 3) % 5) as f64 * 0.01;
                }
                v
            })
            .collect()
    }

    #[test]
    fn kmedoids_is_deterministic_and_covers_clusters() {
        let mut pts = rows(0.0);
        pts.extend(rows(50.0));
        let a = kmedoids(&pts, 2);
        let b = kmedoids(&pts, 2);
        assert_eq!(a, b);
        assert_eq!(a.sizes.iter().sum::<usize>(), 24);
        let (m0, m1) = (a.medoids[0], a.medoids[1]);
        assert_ne!(m0 < 12, m1 < 12, "one medoid per cluster: {a:?}");
        assert_eq!(kmedoids(&pts, 100).medoids.len(), 24);
        assert!(kmedoids(&[], 3).medoids.is_empty());
    }

    #[test]
    fn drift_model_flags_far_vectors_only() {
        let model = DriftModel::fit(&rows(0.0));
        assert!(model.threshold >= MIN_DRIFT_THRESHOLD);
        assert!(!model.check(&rows(0.0)[3]).drifted);
        let far = model.check(&rows(40.0)[3]);
        assert!(far.drifted && far.z_rms > model.threshold);
        assert_eq!(far.worst.len(), 3);
    }

    /// A special project paragraph speaks of lines and empty interiors, never of shading or surfaces, even when the
    /// measured references are smooth shaded bodies (a 0.3.1 plan said "smooth, softly shaded surfaces" right after a
    /// form rule asking for line art with no shading). Raid and doctrine paragraphs describe their processed look.
    #[test]
    fn line_art_and_processed_families_get_paragraphs_that_agree_with_their_form() {
        let smooth = aggregate(&[&analyse(&icon([200, 200, 200], false)).unwrap(), &analyse(&icon([190, 190, 190], false)).unwrap()]);
        assert!(style_paragraph("x", &smooth).contains("softly shaded"), "the generic paragraph describes the surfaces");
        let line = style_paragraph_for("vanilla special project icon art", &smooth, Some(Family::SpecialProjectIcon));
        let lower = line.to_ascii_lowercase();
        assert!(lower.contains("line art") && lower.contains("one pixel") && lower.contains("interior stays empty"), "{line}");
        for w in SHADING_WORDS {
            assert!(!lower.contains(w), "'{w}' in {line}");
        }
        let raid = style_paragraph_for("x", &smooth, Some(Family::Raid));
        assert!(raid.contains("gold gradient") && raid.contains("black outline") && !raid.contains("shaded"), "{raid}");
        let doctrine = style_paragraph_for("x", &smooth, Some(Family::Doctrine));
        assert!(doctrine.contains("units black") && doctrine.contains("glow") && !doctrine.contains("shaded"), "{doctrine}");
        assert_eq!(style_paragraph_for("x", &smooth, Some(Family::Focus)), style_paragraph("x", &smooth));
    }

    #[test]
    fn paragraph_follows_the_measurement() {
        let red = aggregate(&[&analyse(&icon([220, 20, 20], true)).unwrap(), &analyse(&icon([200, 30, 30], true)).unwrap()]);
        let grey = aggregate(&[&analyse(&icon([120, 120, 120], false)).unwrap(), &analyse(&icon([100, 100, 100], false)).unwrap()]);
        let pr = style_paragraph("the mod's focus art", &red);
        let pg = style_paragraph("the mod's focus art", &grey);
        assert!(pr.contains("vivid") && pr.contains("red") && pr.contains("strong dark outline"), "{pr}");
        assert!(pg.contains("muted") && pg.contains("grey") && pg.contains("little or no dark outline"), "{pg}");
    }
    /// A set whose images each have their own colour gets a paragraph that says so instead of naming its averaged
    /// colours as dominant, which pulled every new image of a varied family to one orange-brown. A one-colour set
    /// keeps "dominated by".
    #[test]
    fn a_varied_set_is_not_told_one_dominant_colour() {
        let colours = [[200, 60, 40], [40, 90, 200], [60, 160, 70], [210, 180, 40], [120, 60, 160], [40, 170, 170]];
        let varied: Vec<Analysis> = colours.iter().map(|c| analyse(&icon(*c, true)).unwrap()).collect();
        let same: Vec<Analysis> = [[200, 110, 40], [190, 100, 50], [210, 120, 45]].iter().map(|c| analyse(&icon(*c, true)).unwrap()).collect();
        let v = aggregate(&varied.iter().collect::<Vec<_>>());
        let s = aggregate(&same.iter().collect::<Vec<_>>());
        assert!(v.hue_spread >= VARIED_HUE_SPREAD, "{}", v.hue_spread);
        assert!(s.hue_spread < 0.05, "{}", s.hue_spread);
        let pv = style_paragraph("x", &v);
        let ps = style_paragraph("x", &s);
        assert!(pv.contains("changes from image to image") && !pv.contains("dominated by"), "{pv}");
        assert!(ps.contains("dominated by") && !ps.contains("changes from image to image"), "{ps}");
        // a grey image has no main hue and does not count
        assert_eq!(analyse(&icon([120, 120, 120], false)).unwrap().main_hue, None);
        assert_eq!(hue_spread(&[30.0]), 0.0);
        assert!((hue_spread(&[0.0, 180.0]) - 1.0).abs() < 1e-9);
    }

    /// [`VARIED_HUE_SPREAD`] splits vanilla's families where the docs say: generic focus icons and achievements
    /// share one warm colour, technology pictures and unit leader traits do not. Measured on up to
    /// 150 files a family, every 20th file of the folder in name order.
    #[test]
    fn vanillas_varied_and_one_colour_families_fall_on_either_side_of_the_line() {
        let game = std::env::var("HOI4_GAME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| "C:/Program Files (x86)/Steam/steamapps/common/Hearts of Iron IV".into());
        let spread_of = |dir: &str, prefix: &str| -> Option<(usize, f64)> {
            let mut files: Vec<_> = std::fs::read_dir(game.join(dir))
                .ok()?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "dds") && p.file_name().unwrap().to_string_lossy().starts_with(prefix))
                .collect();
            files.sort();
            let step = (files.len() / 150).max(1);
            let hues: Vec<f64> = files
                .iter()
                .step_by(step)
                .filter_map(|p| crate::dds::decode(&std::fs::read(p).ok()?).ok())
                .filter_map(|i| analyse(&i)?.main_hue)
                .collect();
            (hues.len() >= 10).then(|| (hues.len(), hue_spread(&hues)))
        };
        let Some(goals) = spread_of("gfx/interface/goals", "goal_") else {
            eprintln!("SKIPPED vanilla hue spread: Hearts of Iron IV not found (set HOI4_GAME_DIR); UNAVAILABLE in this run");
            return;
        };
        let tech = spread_of("gfx/interface/technologies", "").unwrap();
        let traits = spread_of("gfx/interface/traits", "").unwrap();
        eprintln!("vanilla hue spread: generic focus {goals:?}, technology {tech:?}, traits {traits:?}");
        assert!(goals.1 < VARIED_HUE_SPREAD, "generic focus icons share one colour: {goals:?}");
        for (name, s) in [("technology", tech), ("traits", traits)] {
            assert!(s.1 >= VARIED_HUE_SPREAD, "{name} vary: {s:?}");
        }
    }
}
