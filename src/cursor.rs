//! Cursor sprites. They are always composited in software onto the lock
//! window; the X pointer itself is a 1x1 transparent cursor.

use crate::render::{Sprite, over, rgb, scale, sd_polygon};
use image::imageops::FilterType;

pub struct CursorSet {
    pub frames: Vec<Sprite>,
    pub frame_ms: u64,
}

impl CursorSet {
    pub fn blank() -> Self {
        CursorSet {
            frames: Vec::new(),
            frame_ms: 0,
        }
    }

    pub fn frame(&self, t_ms: u64) -> Option<&Sprite> {
        match self.frames.len() {
            0 => None,
            1 => Some(&self.frames[0]),
            n => Some(&self.frames[(t_ms / self.frame_ms.max(1)) as usize % n]),
        }
    }

    pub fn animated(&self) -> bool {
        self.frames.len() > 1
    }
}

/// Load a PNG/JPEG with its alpha channel. `size` bounds the larger side.
pub fn load_image(
    path: &str,
    size: Option<u32>,
    hot: (Option<i32>, Option<i32>),
) -> Result<CursorSet, String> {
    let img = crate::render::open_image(path).map_err(|e| format!("cursor image {path}: {e}"))?;
    let mut img = img.to_rgba8();
    if let Some(s) = size {
        let (w, h) = img.dimensions();
        let k = s as f32 / w.max(h) as f32;
        let (nw, nh) = (
            ((w as f32 * k).round() as u32).max(1),
            ((h as f32 * k).round() as u32).max(1),
        );
        img = image::imageops::resize(&img, nw, nh, FilterType::Lanczos3);
    }
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return Err(format!(
            "cursor image {path}: {w}x{h} is outside 1..1024 (use size=N)"
        ));
    }
    let px = img
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            let a32 = a as u32;
            let pm = |c: u8| (c as u32 * a32 + 127) / 255;
            (a32 << 24) | (pm(r) << 16) | (pm(g) << 8) | pm(b)
        })
        .collect();
    let (w, h) = (w as i32, h as i32);
    Ok(CursorSet {
        frames: vec![Sprite {
            w,
            h,
            hot_x: hot.0.unwrap_or(w / 2).clamp(0, w - 1),
            hot_y: hot.1.unwrap_or(h / 2).clamp(0, h - 1),
            px,
        }],
        frame_ms: 0,
    })
}

/// An original "compass star in an orbit ring" emblem with a soft
/// translucent glow. With `pulse`, the glow breathes over 24 frames.
pub fn emblem(size: u32, star: u32, ring: u32, pulse: bool) -> CursorSet {
    let n = if pulse { 24 } else { 1 };
    let frames = (0..n)
        .map(|i| {
            let k = if pulse {
                0.55 + 0.45 * (i as f32 / n as f32 * std::f32::consts::TAU).sin().abs()
            } else {
                0.8
            };
            emblem_frame(size.clamp(16, 512) as i32, rgb(star), rgb(ring), k)
        })
        .collect();
    CursorSet {
        frames,
        frame_ms: 70,
    }
}

fn emblem_frame(size: i32, star: u32, ring: u32, glow: f32) -> Sprite {
    let half = size as f32 / 2.0;
    // four long points (N/E/S/W), short waist on the diagonals
    let mut pts = Vec::with_capacity(8);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let r = if i % 2 == 0 { 0.97 } else { 0.22 };
        pts.push((a.sin() * r * half, -a.cos() * r * half));
    }
    let lit = star;
    let shade = crate::render::dim(star, 0.62);
    let mut px = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
            let d = (fx * fx + fy * fy).sqrt();
            let u = d / half;
            let mut c = 0u32;
            // translucent glow, fades to nothing at the edge
            if u < 1.0 {
                c = over(c, scale(ring, glow * 0.38 * (1.0 - u).powi(2)));
            }
            // orbit ring
            let rr = 0.70 * half;
            let rw = (0.045 * half).max(1.0);
            let cov = (0.5 - ((d - rr).abs() - rw)).clamp(0.0, 1.0);
            if cov > 0.0 {
                c = over(c, scale(ring, cov * 0.9));
            }
            // dark outline keeps the star readable on bright backgrounds
            let sd = sd_polygon(fx, fy, &pts);
            let cov = (0.5 - (sd - 1.6)).clamp(0.0, 1.0);
            if cov > 0.0 {
                c = over(c, scale(0xff00_0000, cov * 0.7));
            }
            // faceted star: each point lit on one side, shaded on the other
            let cov = (0.5 - sd).clamp(0.0, 1.0);
            if cov > 0.0 {
                let ang = fx.atan2(-fy).rem_euclid(std::f32::consts::TAU);
                let facet = (ang / std::f32::consts::FRAC_PI_2).fract();
                let col = if facet < 0.5 { lit } else { shade };
                c = over(c, scale(col, cov));
            }
            // hub
            let cov = (0.5 - (d - 0.075 * half)).clamp(0.0, 1.0);
            if cov > 0.0 {
                c = over(c, scale(0xff10_1018, cov));
            }
            px[(y * size + x) as usize] = c;
        }
    }
    Sprite {
        w: size,
        h: size,
        hot_x: size / 2,
        hot_y: size / 2,
        px,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emblem_has_alpha_layers() {
        let c = emblem(64, 0xffcc66, 0x99ccff, true);
        assert_eq!(c.frames.len(), 24);
        let s = &c.frames[0];
        let alphas: std::collections::BTreeSet<u32> = s.px.iter().map(|p| p >> 24).collect();
        assert!(alphas.contains(&0), "corners transparent");
        assert!(alphas.contains(&255), "star opaque");
        assert!(
            alphas.iter().any(|a| *a > 0 && *a < 255),
            "translucent glow"
        );
        // premultiplied: no channel exceeds alpha
        for p in &s.px {
            let a = p >> 24;
            assert!((p >> 16 & 0xff) <= a && (p >> 8 & 0xff) <= a && (p & 0xff) <= a);
        }
    }
}
