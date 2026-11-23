//! Lock-screen themes. A theme draws everything under the software cursor:
//! the scene, its animation, and the authentication feedback.
//!
//! Adding a theme:
//! 1. write `theme/NAME.rs` with an options type implementing [`Spec`] and a
//!    state type implementing [`Theme`];
//! 2. add one [`Entry`] to [`REGISTRY`].
//!
//! The locker never looks inside a theme. It hands over the canvas, the
//! auth state ([`Ui`]) and pointer clicks, and pushes whatever the theme
//! reports as damaged.

pub mod console;
pub mod plain;

use crate::args::FrameCfg;
use crate::render::{Canvas, Rect};
use crate::ui::Ui;

/// What a theme gets to know about the screen when it is built.
pub struct Env<'a> {
    /// RandR monitors, primary first. Never empty.
    pub monitors: &'a [Rect],
    /// Random seed for procedural content.
    pub seed: u64,
    /// The `-input` indicator preference. Themes with their own auth panel
    /// may ignore it.
    pub input: Option<&'a FrameCfg>,
}

/// Parsed `-bg` options. `build` runs before the lock window is mapped and
/// again whenever the monitor layout changes. If it fails, the locker falls
/// back to a blank screen rather than not locking.
pub trait Spec {
    fn build(&self, env: &Env) -> Result<Box<dyn Theme>, String>;
}

/// A running theme. Times are milliseconds since the locker started.
pub trait Theme {
    /// Paint the whole scene into `c` (first frame).
    fn draw_all(&mut self, c: &mut Canvas, t: u64, ui: &Ui);

    /// Advance animations, redraw what changed into `c` and push those
    /// areas to `dmg`. Returns when the theme next wants a tick, or `None`
    /// if it only changes on input.
    fn tick(&mut self, c: &mut Canvas, t: u64, ui: &Ui, dmg: &mut Vec<Rect>) -> Option<u64>;

    /// Areas the auth overlay covers. They are repainted whenever the auth
    /// state changes.
    fn overlay_rects(&self) -> Vec<Rect>;

    /// Draw the auth overlay on top of the scene. `c` is clipped to the area
    /// being repainted; draw everything and let the clip do the work.
    fn overlay(&self, c: &mut Canvas, t: u64, ui: &Ui);

    /// Left button pressed / released. Purely cosmetic: themes never see
    /// key presses and cannot affect authentication.
    fn press(&mut self, _x: i32, _y: i32, _t: u64) {}
    fn release(&mut self, _x: i32, _y: i32, _t: u64) {}
}

/// `type:k=v,flag,...` options after the type name.
pub type Opts<'a> = [(&'a str, Option<&'a str>)];

pub struct Entry {
    pub name: &'static str,
    /// Usage line after `-bg `.
    pub usage: &'static str,
    pub parse: fn(&Opts) -> Result<Box<dyn Spec>, String>,
}

pub const REGISTRY: &[Entry] = &[
    Entry {
        name: "blank",
        usage: "blank[:color=C]",
        parse: plain::parse_blank,
    },
    Entry {
        name: "image",
        usage: "image:file=F[,fill|scale|center|tiled][,color=C][,shade=0-99]",
        parse: plain::parse_image,
    },
    Entry {
        name: "console",
        usage: "console[:palette=amber|blue,cycle=SECS,fps=N,title=TEXT]",
        parse: console::parse,
    },
];

pub fn parse(name: &str, opts: &Opts) -> Result<Box<dyn Spec>, String> {
    match REGISTRY.iter().find(|e| e.name == name) {
        Some(e) => (e.parse)(opts),
        None => {
            let names: Vec<_> = REGISTRY.iter().map(|e| e.name).collect();
            Err(format!("-bg: unknown type {name} ({})", names.join(", ")))
        }
    }
}

pub fn need<'a>(k: &str, v: Option<&'a str>) -> Result<&'a str, String> {
    v.ok_or(format!("option {k} needs a value"))
}

pub fn num<T: std::str::FromStr>(k: &str, v: Option<&str>) -> Result<T, String> {
    need(k, v)?
        .parse()
        .map_err(|_| format!("option {k}: not a number"))
}

/// Wraps a theme so that a panic inside it cannot end the process, which
/// for a screen locker would mean unlocking. The first panic replaces the
/// theme with a blank screen for the rest of the session.
pub struct Guarded {
    inner: Box<dyn Theme>,
    monitors: Vec<Rect>,
    frame: Option<FrameCfg>,
    failed: std::cell::Cell<bool>,
}

impl Guarded {
    pub fn new(inner: Box<dyn Theme>, monitors: &[Rect], frame: Option<FrameCfg>) -> Self {
        Guarded {
            inner,
            monitors: monitors.to_vec(),
            frame,
            failed: std::cell::Cell::new(false),
        }
    }

    /// Build `spec`, or the blank screen if that fails or panics.
    pub fn build(spec: &dyn Spec, env: &Env) -> Self {
        let frame = env.input.copied();
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spec.build(env)))
            .unwrap_or_else(|_| Err("the theme panicked".into()));
        let inner = built.unwrap_or_else(|e| {
            eprintln!("alock-rs: {e}; locking with a blank screen");
            Box::new(plain::Plain::blank(env.monitors, frame))
        });
        Guarded::new(inner, env.monitors, frame)
    }

    fn guard<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok();
        if r.is_none() {
            self.failed.set(true);
        }
        r
    }

    /// After a panic: swap in the blank screen and repaint everything.
    /// Returns true if it did.
    fn recover(&mut self, c: &mut Canvas, t: u64, ui: &Ui) -> bool {
        if !self.failed.replace(false) {
            return false;
        }
        eprintln!("alock-rs: the theme panicked; continuing with a blank screen");
        self.inner = Box::new(plain::Plain::blank(&self.monitors, self.frame));
        c.set_clip(None);
        self.inner.draw_all(c, t, ui);
        true
    }
}

impl Theme for Guarded {
    fn draw_all(&mut self, c: &mut Canvas, t: u64, ui: &Ui) {
        let inner = &mut self.inner;
        let ok =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.draw_all(c, t, ui)));
        if ok.is_err() {
            self.failed.set(true);
        }
        self.recover(c, t, ui);
    }

    fn tick(&mut self, c: &mut Canvas, t: u64, ui: &Ui, dmg: &mut Vec<Rect>) -> Option<u64> {
        let mut next = None;
        if !self.failed.get() {
            let inner = &mut self.inner;
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                inner.tick(c, t, ui, dmg)
            })) {
                Ok(n) => next = n,
                Err(_) => self.failed.set(true),
            }
        }
        if self.recover(c, t, ui) {
            dmg.push(c.r);
        }
        next
    }

    fn overlay_rects(&self) -> Vec<Rect> {
        self.guard(|| self.inner.overlay_rects())
            .unwrap_or_default()
    }

    fn overlay(&self, c: &mut Canvas, t: u64, ui: &Ui) {
        if !self.failed.get() {
            self.guard(|| self.inner.overlay(c, t, ui));
        }
    }

    fn press(&mut self, x: i32, y: i32, t: u64) {
        let inner = &mut self.inner;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.press(x, y, t))).is_err()
        {
            self.failed.set(true);
        }
    }

    fn release(&mut self, x: i32, y: i32, t: u64) {
        let inner = &mut self.inner;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.release(x, y, t)))
            .is_err()
        {
            self.failed.set(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bomb;
    impl Theme for Bomb {
        fn draw_all(&mut self, _: &mut Canvas, _: u64, _: &Ui) {}
        fn tick(&mut self, _: &mut Canvas, _: u64, _: &Ui, _: &mut Vec<Rect>) -> Option<u64> {
            Some(1)
        }
        fn overlay_rects(&self) -> Vec<Rect> {
            Vec::new()
        }
        fn overlay(&self, _: &mut Canvas, _: u64, _: &Ui) {}
        fn press(&mut self, _: i32, _: i32, _: u64) {
            panic!("boom");
        }
    }

    #[test]
    fn a_panicking_theme_becomes_a_blank_screen() {
        let mon = Rect::new(0, 0, 64, 48);
        let mut c = Canvas::new(mon, 0xffff_ffff);
        let ui = Ui::new();
        let mut g = Guarded::new(Box::new(Bomb), &[mon], None);
        let mut dmg = Vec::new();
        assert_eq!(g.tick(&mut c, 0, &ui, &mut dmg), Some(1));
        g.press(1, 1, 5);
        // the next tick repaints the whole screen black and goes idle
        assert_eq!(g.tick(&mut c, 10, &ui, &mut dmg), None);
        assert_eq!(dmg, vec![mon]);
        assert!(c.px.iter().all(|&p| p == 0xff00_0000));
        g.press(1, 1, 20);
        g.overlay(&mut c, 20, &ui);
    }
}
