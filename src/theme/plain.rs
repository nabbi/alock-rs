//! The alock-style themes: a solid color or an image, with the coloured
//! `-input frame` around each monitor and PAM messages in a pill.
//! Nothing animates, so the screen only changes on input.

use super::{Env, Opts, Spec, Theme, need, num};
use crate::args::{FrameCfg, parse_color};
use crate::background::{ImageBg, ImageMode};
use crate::render::{Canvas, Rect, rgb, scale, text_width};
use crate::ui::{Phase, Ui};

const BLACK: u32 = 0xff00_0000;

pub enum Backdrop {
    Blank {
        color: u32,
    },
    Image {
        file: String,
        mode: ImageMode,
        color: u32,
        shade: u8,
    },
}

impl Spec for Backdrop {
    fn build(&self, env: &Env) -> Result<Box<dyn Theme>, String> {
        let bg = match self {
            Backdrop::Blank { color } => Err(*color),
            Backdrop::Image {
                file,
                mode,
                color,
                shade,
            } => Ok(ImageBg::load(file, *mode, *color, *shade)?),
        };
        Ok(Box::new(Plain {
            bg,
            monitors: env.monitors.to_vec(),
            frame: env.input.copied(),
        }))
    }
}

pub fn parse_blank(opts: &Opts) -> Result<Box<dyn Spec>, String> {
    let mut color = 0;
    for &(k, v) in opts {
        match k {
            "color" => color = parse_color(need(k, v)?)?,
            _ => return Err(format!("-bg blank: unknown option {k}")),
        }
    }
    Ok(Box::new(Backdrop::Blank { color }))
}

pub fn parse_image(opts: &Opts) -> Result<Box<dyn Spec>, String> {
    parse_image_opts(opts).map(|b| Box::new(b) as Box<dyn Spec>)
}

fn parse_image_opts(opts: &Opts) -> Result<Backdrop, String> {
    let (mut file, mut mode, mut color, mut shade) = (None, ImageMode::Fill, 0, 0u8);
    for &(k, v) in opts {
        match k {
            "file" => file = Some(need(k, v)?.to_string()),
            "fill" => mode = ImageMode::Fill,
            "scale" => mode = ImageMode::Scale,
            "center" => mode = ImageMode::Center,
            "tiled" => mode = ImageMode::Tiled,
            "color" => color = parse_color(need(k, v)?)?,
            "shade" => shade = num::<u8>(k, v)?.min(99),
            _ => return Err(format!("-bg image: unknown option {k}")),
        }
    }
    Ok(Backdrop::Image {
        file: file.ok_or("-bg image needs file=")?,
        mode,
        color,
        shade,
    })
}

pub struct Plain {
    /// The image, or the solid color when there is none.
    bg: Result<ImageBg, u32>,
    monitors: Vec<Rect>,
    frame: Option<FrameCfg>,
}

impl Plain {
    /// The black screen the locker falls back to when a theme fails.
    pub fn blank(monitors: &[Rect], frame: Option<FrameCfg>) -> Self {
        Plain {
            bg: Err(0),
            monitors: monitors.to_vec(),
            frame,
        }
    }

    fn message_rect(&self) -> Rect {
        let m = self.monitors[0];
        let h = 7 * 4 + 24;
        Rect::new(m.x, m.bottom() - m.h / 10 - h, m.w, h)
    }
}

impl Theme for Plain {
    fn draw_all(&mut self, c: &mut Canvas, _t: u64, _ui: &Ui) {
        match &self.bg {
            Err(color) => c.fill_rect(c.r, rgb(*color)),
            Ok(img) => {
                c.fill_rect(c.r, BLACK);
                for m in &self.monitors {
                    img.draw(c, *m);
                }
            }
        }
    }

    fn tick(&mut self, _c: &mut Canvas, _t: u64, _ui: &Ui, _dmg: &mut Vec<Rect>) -> Option<u64> {
        None
    }

    fn overlay_rects(&self) -> Vec<Rect> {
        let mut v = vec![self.message_rect()];
        if let Some(f) = &self.frame {
            for m in &self.monitors {
                v.extend(frame_rects(*m, f.width));
            }
        }
        v
    }

    fn overlay(&self, c: &mut Canvas, _t: u64, ui: &Ui) {
        if let Some(f) = &self.frame {
            let col = match ui.phase {
                Phase::Idle => None,
                Phase::Typing => Some(f.input),
                Phase::Checking => Some(f.check),
                Phase::Denied => Some(f.error),
            };
            if let Some(col) = col {
                for m in &self.monitors {
                    for r in frame_rects(*m, f.width) {
                        c.fill_rect(r, rgb(col));
                    }
                }
            }
        }
        if let Some(msg) = &ui.message {
            let r = self.message_rect();
            let (sx, sy) = (3, 4);
            let w = text_width(msg, sx);
            let pill = Rect::new(r.x + (r.w - w) / 2 - 24, r.y, w + 48, r.h);
            c.pill(pill, scale(BLACK, 0.7));
            c.text(pill.x + 24, r.y + 12, msg, sx, sy, 0xffff_ffff);
        }
    }
}

fn frame_rects(m: Rect, w: i32) -> [Rect; 4] {
    [
        Rect::new(m.x, m.y, m.w, w),
        Rect::new(m.x, m.bottom() - w, m.w, w),
        Rect::new(m.x, m.y, w, m.h),
        Rect::new(m.right() - w, m.y, w, m.h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_options() {
        let b = parse_image_opts(&[("file", Some("/x/lock")), ("shade", Some("30"))]).unwrap();
        assert!(matches!(
            b,
            Backdrop::Image { ref file, mode: ImageMode::Fill, shade: 30, .. } if file == "/x/lock"
        ));
        assert!(parse_image_opts(&[("shade", Some("30"))]).is_err());
    }

    #[test]
    fn frame_follows_phase() {
        let mon = Rect::new(0, 0, 200, 100);
        let frame = FrameCfg {
            width: 4,
            input: 0x112233,
            check: 0x445566,
            error: 0x778899,
        };
        let env = Env {
            monitors: &[mon],
            seed: 0,
            input: Some(&frame),
        };
        let mut th = Backdrop::Blank { color: 0 }.build(&env).unwrap();
        let mut c = Canvas::new(mon, BLACK);
        let mut ui = Ui::new();
        th.draw_all(&mut c, 0, &ui);
        th.overlay(&mut c, 0, &ui);
        assert_eq!(c.px[0], BLACK);
        ui.phase = Phase::Typing;
        th.overlay(&mut c, 0, &ui);
        assert_eq!(c.px[0], rgb(0x112233));
    }
}
