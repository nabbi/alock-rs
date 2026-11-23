//! Static backgrounds: a solid color or an image, laid out per monitor.

use crate::render::{Canvas, Rect, over, rgb, scale};
use image::RgbaImage;
use image::imageops::FilterType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMode {
    /// Scale to cover the monitor, cropping the overflow (default).
    Fill,
    /// Stretch to the monitor size, ignoring aspect ratio.
    Scale,
    /// Natural size, centered.
    Center,
    /// Natural size, repeated.
    Tiled,
}

pub struct ImageBg {
    pub img: RgbaImage,
    pub mode: ImageMode,
    pub color: u32,
    pub shade: u8,
}

impl ImageBg {
    pub fn load(path: &str, mode: ImageMode, color: u32, shade: u8) -> Result<Self, String> {
        let img =
            crate::render::open_image(path).map_err(|e| format!("background image {path}: {e}"))?;
        Ok(ImageBg {
            img: img.to_rgba8(),
            mode,
            color,
            shade: shade.min(99),
        })
    }

    pub fn draw(&self, c: &mut Canvas, mon: Rect) {
        c.fill_rect(mon, rgb(self.color));
        let (iw, ih) = (self.img.width() as i32, self.img.height() as i32);
        if iw == 0 || ih == 0 || mon.is_empty() {
            return;
        }
        match self.mode {
            ImageMode::Fill => {
                // Crop to the monitor's aspect first, then scale: the scaled
                // image is never larger than the monitor, whatever the
                // aspect ratio of the file.
                let (iw, ih) = (iw as u64, ih as u64);
                let (mw, mh) = (mon.w as u64, mon.h as u64);
                let (cw, ch) = if iw * mh > ih * mw {
                    ((ih * mw / mh).clamp(1, iw), ih)
                } else {
                    (iw, (iw * mh / mw).clamp(1, ih))
                };
                let crop = image::imageops::crop_imm(
                    &self.img,
                    ((iw - cw) / 2) as u32,
                    ((ih - ch) / 2) as u32,
                    cw as u32,
                    ch as u32,
                )
                .to_image();
                let s = image::imageops::resize(
                    &crop,
                    mon.w as u32,
                    mon.h as u32,
                    FilterType::Triangle,
                );
                blit(c, &s, mon.x, mon.y, mon);
            }
            ImageMode::Scale => {
                let s = image::imageops::resize(
                    &self.img,
                    mon.w as u32,
                    mon.h as u32,
                    FilterType::Triangle,
                );
                blit(c, &s, mon.x, mon.y, mon);
            }
            ImageMode::Center => {
                blit(
                    c,
                    &self.img,
                    mon.x + (mon.w - iw) / 2,
                    mon.y + (mon.h - ih) / 2,
                    mon,
                );
            }
            ImageMode::Tiled => {
                let mut y = mon.y;
                while y < mon.bottom() {
                    let mut x = mon.x;
                    while x < mon.right() {
                        blit(c, &self.img, x, y, mon);
                        x += iw;
                    }
                    y += ih;
                }
            }
        }
        if self.shade > 0 {
            c.fill_rect(mon, scale(rgb(self.color), self.shade as f32 / 100.0));
        }
    }
}

/// Draw `img` with its top-left at (x, y), clipped to `clip`.
fn blit(c: &mut Canvas, img: &RgbaImage, x: i32, y: i32, clip: Rect) {
    let r = Rect::new(x, y, img.width() as i32, img.height() as i32)
        .intersect(&clip)
        .intersect(&c.clip());
    for py in r.y..r.bottom() {
        for px in r.x..r.right() {
            let [cr, cg, cb, ca] = img.get_pixel((px - x) as u32, (py - y) as u32).0;
            let a = ca as u32;
            let pm = |v: u8| (v as u32 * a + 127) / 255;
            let src = (a << 24) | (pm(cr) << 16) | (pm(cg) << 8) | pm(cb);
            let i = (py - c.r.y) as usize * c.r.w as usize + (px - c.r.x) as usize;
            c.px[i] = over(c.px[i], src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_handles_extreme_aspect_ratios() {
        let mon = Rect::new(10, 0, 200, 100);
        for (w, h) in [(1, 4000), (4000, 1), (1, 1), (200, 100), (333, 77)] {
            let bg = ImageBg {
                img: RgbaImage::from_pixel(w, h, image::Rgba([10, 200, 30, 255])),
                mode: ImageMode::Fill,
                color: 0,
                shade: 0,
            };
            let mut c = Canvas::new(Rect::new(0, 0, 220, 100), 0xff00_0000);
            bg.draw(&mut c, mon);
            // the monitor is covered edge to edge, and nothing outside it
            assert_eq!(c.px[10], 0xff0a_c81e, "{w}x{h}");
            assert_eq!(c.px[99 * 220 + 209], 0xff0a_c81e, "{w}x{h}");
            assert_eq!(c.px[0], 0xff00_0000, "{w}x{h}");
        }
    }
}
