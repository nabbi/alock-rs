//! Tiny software rasterizer. Everything the lock screen shows, including the
//! cursor, is composited here into client-side pixels and pushed to one X
//! window. Nothing uses the hardware cursor plane, which is what kept bleeding
//! stale desktop content through ARGB cursors on NVIDIA after resume.
//!
//! Pixels are premultiplied ARGB `0xAARRGGBB`.

use crate::font;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn area(&self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.w as i64 * self.h as i64
        }
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = self.right().max(o.right());
        let y1 = self.bottom().max(o.bottom());
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
    pub fn overlaps(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
}

/// Opaque color from 0xRRGGBB.
pub const fn rgb(c: u32) -> u32 {
    0xff00_0000 | (c & 0x00ff_ffff)
}

/// Multiply every channel (alpha included) by `k` in 0..=1.
pub fn scale(c: u32, k: f32) -> u32 {
    let k = (k.clamp(0.0, 1.0) * 256.0) as u32;
    let rb = (((c & 0x00ff_00ff) * k) >> 8) & 0x00ff_00ff;
    let ag = (((c >> 8) & 0x00ff_00ff) * k) & 0xff00_ff00;
    rb | ag
}

/// Darken an opaque color, keeping it opaque.
pub fn dim(c: u32, k: f32) -> u32 {
    scale(c, k) | 0xff00_0000
}

/// Mix two opaque colors.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let x = ((a >> s) & 0xff) as f32;
        let y = ((b >> s) & 0xff) as f32;
        ((x + (y - x) * t).round() as u32) << s
    };
    0xff00_0000 | ch(16) | ch(8) | ch(0)
}

/// Porter-Duff source-over for premultiplied pixels.
#[inline]
pub fn over(dst: u32, src: u32) -> u32 {
    let sa = src >> 24;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    // exact x*inv/255 with rounding, two 8-bit lanes at a time
    let rb = (dst & 0x00ff_00ff) * inv + 0x0080_0080;
    let rb = ((rb + ((rb >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let ag = ((dst >> 8) & 0x00ff_00ff) * inv + 0x0080_0080;
    let ag = (ag + ((ag >> 8) & 0x00ff_00ff)) & 0xff00_ff00;
    src + (rb | ag)
}

/// Premultiplied ARGB image with a hotspot (used for cursors).
#[derive(Clone)]
pub struct Sprite {
    pub w: i32,
    pub h: i32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub px: Vec<u32>,
}

/// A rectangle of pixels positioned in screen coordinates. Drawing is
/// limited to `clip` (the whole canvas unless narrowed with `set_clip`).
pub struct Canvas {
    pub r: Rect,
    pub px: Vec<u32>,
    clip: Rect,
}

impl Canvas {
    pub fn new(r: Rect, fill: u32) -> Self {
        let n = (r.w.max(0) as usize) * (r.h.max(0) as usize);
        Canvas {
            r,
            px: vec![fill; n],
            clip: r,
        }
    }

    /// Limit drawing to `r` (intersected with the canvas). `None` resets it.
    pub fn set_clip(&mut self, r: Option<Rect>) {
        self.clip = r.map_or(self.r, |r| r.intersect(&self.r));
    }

    pub fn clip(&self) -> Rect {
        self.clip
    }

    #[inline]
    fn idx(&self, x: i32, y: i32) -> usize {
        (y - self.r.y) as usize * self.r.w as usize + (x - self.r.x) as usize
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: u32) {
        if self.clip.contains(x, y) {
            let i = self.idx(x, y);
            self.px[i] = over(self.px[i], c);
        }
    }

    pub fn fill_rect(&mut self, r: Rect, c: u32) {
        let r = r.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        for y in r.y..r.bottom() {
            let i = self.idx(r.x, y);
            let row = &mut self.px[i..i + r.w as usize];
            if c >> 24 == 255 {
                row.fill(c);
            } else {
                for p in row {
                    *p = over(*p, c);
                }
            }
        }
    }

    /// Copy the overlapping area of `src` into self.
    pub fn copy_from(&mut self, src: &Canvas) {
        let r = self.clip.intersect(&src.r);
        if r.is_empty() {
            return;
        }
        for y in r.y..r.bottom() {
            let d = self.idx(r.x, y);
            let s = src.idx(r.x, y);
            self.px[d..d + r.w as usize].copy_from_slice(&src.px[s..s + r.w as usize]);
        }
    }

    /// Fill a shape given by a signed distance function (pixels, negative
    /// inside) evaluated at pixel centers, anti-aliased by coverage.
    pub fn fill_sdf(&mut self, bbox: Rect, c: u32, f: impl Fn(f32, f32) -> f32) {
        let r = bbox.intersect(&self.clip);
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let cov = (0.5 - f(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let i = self.idx(x, y);
                    self.px[i] = over(self.px[i], if cov >= 1.0 { c } else { scale(c, cov) });
                }
            }
        }
    }

    /// Rounded rectangle; radii are top-left, top-right, bottom-right, bottom-left.
    pub fn round_rect(&mut self, r: Rect, radii: [f32; 4], c: u32) {
        let (x0, y0, x1, y1) = (r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32);
        self.fill_sdf(r, c, |x, y| sd_round_box(x, y, x0, y0, x1, y1, radii));
    }

    /// Horizontal pill (fully rounded ends).
    pub fn pill(&mut self, r: Rect, c: u32) {
        let rad = r.h as f32 / 2.0;
        self.round_rect(r, [rad; 4], c);
    }

    /// Anti-aliased ring (or disc when `r_in` <= 0), drawn with row spans so
    /// only pixels near the shape are evaluated.
    pub fn ring(&mut self, cx: f32, cy: f32, r_out: f32, r_in: f32, c: u32) {
        let clip = self.clip;
        let y0 = ((cy - r_out - 1.0).floor() as i32).max(clip.y);
        let y1 = ((cy + r_out + 1.0).ceil() as i32).min(clip.bottom());
        let ro = r_out + 1.0;
        let ri = r_in - 1.0;
        for y in y0..y1 {
            let py = y as f32 + 0.5 - cy;
            if py.abs() > ro {
                continue;
            }
            let xo = (ro * ro - py * py).max(0.0).sqrt();
            let xi = if ri > 0.0 && py.abs() < ri {
                (ri * ri - py * py).sqrt()
            } else {
                -1.0
            };
            let mut span = |a: f32, b: f32| {
                let xa = ((cx + a).floor() as i32).max(clip.x);
                let xb = ((cx + b).ceil() as i32).min(clip.right());
                for x in xa..xb {
                    let px = x as f32 + 0.5 - cx;
                    let d = (px * px + py * py).sqrt();
                    let sd = (d - r_out).max(r_in - d);
                    let cov = (0.5 - sd).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let i = self.idx(x, y);
                        self.px[i] = over(self.px[i], if cov >= 1.0 { c } else { scale(c, cov) });
                    }
                }
            };
            if xi < 0.0 {
                span(-xo, xo);
            } else {
                span(-xo, -xi);
                span(xi, xo);
            }
        }
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, c: u32) {
        let m = w + 2.0;
        let bb = Rect::new(
            (x0.min(x1) - m) as i32,
            (y0.min(y1) - m) as i32,
            ((x0 - x1).abs() + 2.0 * m) as i32 + 1,
            ((y0 - y1).abs() + 2.0 * m) as i32 + 1,
        );
        self.fill_sdf(bb, c, |x, y| sd_segment(x, y, x0, y0, x1, y1) - w / 2.0);
    }

    pub fn sprite(&mut self, s: &Sprite, x: i32, y: i32) {
        let dst = Rect::new(x, y, s.w, s.h).intersect(&self.clip);
        for py in dst.y..dst.bottom() {
            let srow = ((py - y) * s.w) as usize;
            for px in dst.x..dst.right() {
                let c = s.px[srow + (px - x) as usize];
                if c >> 24 != 0 {
                    let i = self.idx(px, py);
                    self.px[i] = over(self.px[i], c);
                }
            }
        }
    }

    /// Draw text with the built-in 5x7 font scaled by (sx, sy). Returns width.
    pub fn text(&mut self, x: i32, y: i32, s: &str, sx: i32, sy: i32, c: u32) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            if let Some(rows) = font::glyph(ch) {
                for (ry, bits) in rows.iter().enumerate() {
                    for col in 0..5 {
                        if bits & (0x10 >> col) != 0 {
                            self.fill_rect(Rect::new(cx + col * sx, y + ry as i32 * sy, sx, sy), c);
                        }
                    }
                }
            }
            cx += 6 * sx;
        }
        (cx - x - sx).max(0)
    }
}

pub fn text_width(s: &str, sx: i32) -> i32 {
    (s.chars().count() as i32 * 6 * sx - sx).max(0)
}

/// Signed distance to a box with per-corner radii (tl, tr, br, bl), y down.
pub fn sd_round_box(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, radii: [f32; 4]) -> f32 {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hw = (x1 - x0) * 0.5;
    let hh = (y1 - y0) * 0.5;
    let qx = px - cx;
    let qy = py - cy;
    let r = match (qx > 0.0, qy > 0.0) {
        (false, false) => radii[0],
        (true, false) => radii[1],
        (true, true) => radii[2],
        (false, true) => radii[3],
    }
    .min(hw)
    .min(hh);
    let dx = qx.abs() - hw + r;
    let dy = qy.abs() - hh + r;
    dx.max(dy).min(0.0) + (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() - r
}

pub fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (wx, wy) = (px - ax, py - ay);
    let (ex, ey) = (bx - ax, by - ay);
    let l = ex * ex + ey * ey;
    let h = if l > 0.0 {
        ((wx * ex + wy * ey) / l).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy) = (wx - ex * h, wy - ey * h);
    (dx * dx + dy * dy).sqrt()
}

/// Signed distance to a closed polygon.
pub fn sd_polygon(px: f32, py: f32, v: &[(f32, f32)]) -> f32 {
    let n = v.len();
    let mut d = (px - v[0].0).powi(2) + (py - v[0].1).powi(2);
    let mut s = 1.0f32;
    let mut j = n - 1;
    for i in 0..n {
        let (ex, ey) = (v[j].0 - v[i].0, v[j].1 - v[i].1);
        let (wx, wy) = (px - v[i].0, py - v[i].1);
        let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        let (bx, by) = (wx - ex * t, wy - ey * t);
        d = d.min(bx * bx + by * by);
        let c1 = py >= v[i].1;
        let c2 = py < v[j].1;
        let c3 = ex * wy > ey * wx;
        if (c1 && c2 && c3) || (!c1 && !c2 && !c3) {
            s = -s;
        }
        j = i;
    }
    s * d.sqrt()
}

/// Largest accepted image side; with the allocation cap this bounds what a
/// hostile or broken file can make the decoder do.
const MAX_IMAGE_SIDE: u32 = 16384;

/// Decode an image by sniffing its content, so extensionless files and
/// symlinks like `~/.images/lock` work.
pub fn open_image(path: &str) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}

/// Cheap deterministic hash for procedural animation.
pub fn hash(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

pub fn hash2(a: u64, b: u64) -> u64 {
    hash(a ^ hash(b))
}

/// Uniform float in [0, 1) from a hash.
pub fn unit(h: u64) -> f32 {
    (h >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_is_bounded() {
        for sa in [0u32, 1, 64, 128, 254, 255] {
            let src = scale(0xffff_ffff, sa as f32 / 255.0);
            let out = over(0xffff_ffff, src);
            assert_eq!(out >> 24, 0xff);
        }
        assert_eq!(over(0xff00_0000, 0xffff_0000), 0xffff_0000);
        assert_eq!(over(0xff12_3456, 0), 0xff12_3456);
    }

    #[test]
    fn rect_ops() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
    }

    #[test]
    fn canvas_clips() {
        let mut c = Canvas::new(Rect::new(10, 10, 4, 4), 0xff00_0000);
        c.fill_rect(Rect::new(0, 0, 12, 12), 0xffff_ffff);
        assert_eq!(c.px[0], 0xffff_ffff);
        assert_eq!(c.px[2], 0xff00_0000);
        c.text(-100, -100, "OFFSCREEN", 3, 3, 0xffff_ffff);

        c.set_clip(Some(Rect::new(10, 10, 1, 1)));
        c.fill_rect(c.r, 0xff11_1111);
        assert_eq!(c.px[0], 0xff11_1111);
        assert_eq!(c.px[1], 0xffff_ffff);
        c.set_clip(None);
        assert_eq!(c.clip(), c.r);
    }
}
