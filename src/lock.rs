//! X11 side: one override-redirect window over the whole screen, keyboard and
//! pointer grabs, and an event loop that composites the scene, the auth
//! overlay and the software cursor into that window.
//!
//! The pointer is grabbed with a fully transparent 1x1 cursor so the hardware
//! cursor plane never holds alpha pixels (the NVIDIA resume bleed-through).
//!
//! Once the screen is locked, the only ways out are a successful
//! authentication and losing the X server. Bad images, a panicking theme, a
//! dying auth helper or a changing monitor layout all degrade to a blank,
//! still locked screen.

use crate::args::{AuthCfg, Config, CursorCfg, FrameCfg};
use crate::auth::{self, AuthEvent, Authenticator, Session};
use crate::cursor::{self, CursorSet};
use crate::keys::{Key, Keymap};
use crate::present::Presenter;
use crate::render::{Canvas, Rect};
use crate::theme::{Env, Guarded, Spec, Theme};
use crate::ui::{Phase, Ui};
use std::error::Error;
use std::os::fd::AsRawFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::dpms::{self, ConnectionExt as _};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use zeroize::{Zeroize, Zeroizing};

const BLACK: u32 = 0xff00_0000;
const TYPING_TIMEOUT_MS: u64 = 15_000;
const DENIED_MS: u64 = 2_500;
const MESSAGE_MS: u64 = 10_000;
const MAX_PASSWORD: usize = 255;
/// A helper that has not finished or asked for input by then is killed.
const AUTH_TIMEOUT_MS: u64 = 120_000;
/// At most one re-raise per this long, so two clients that both insist on
/// being on top cannot spin the X server.
const RAISE_MS: u64 = 100;

struct Locker {
    conn: RustConnection,
    root: Window,
    win: Window,
    full: Rect,
    monitors: Vec<Rect>,
    spec: Box<dyn Spec>,
    frame: Option<FrameCfg>,
    seed: u64,
    /// What the theme drew.
    scene: Canvas,
    /// Scene + auth overlay + cursor, as last sent to the window.
    out: Canvas,
    theme: Guarded,
    presenter: Presenter,
    cursor: CursorSet,
    ptr: (i32, i32),
    cursor_drawn: Option<(Rect, usize)>,
    ui: Ui,
    damage: Vec<Rect>,
    epoch: Instant,
}

pub fn run(cfg: Config) -> Result<(), Box<dyn Error>> {
    // A cosmetic problem must never be the reason the screen stays
    // unlocked: a cursor or background that cannot be loaded is replaced by
    // the built-in one, with a warning.
    let cursor = load_cursor(cfg.cursor);
    let auth = match cfg.auth {
        AuthCfg::Pam { service } => Authenticator::pam(service.as_deref())?,
        #[cfg(feature = "test-auth")]
        AuthCfg::Test(p) => Authenticator::Test(p),
    };
    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = conn.setup().roots[screen_num].clone();
    let swap = check_visual(&conn, &screen);
    let full = Rect::new(
        0,
        0,
        screen.width_in_pixels as i32,
        screen.height_in_pixels as i32,
    );
    let monitors = monitors(&conn, screen.root, full);
    let epoch = Instant::now();
    let seed = epoch.elapsed().as_nanos() as u64 ^ std::process::id() as u64;
    let theme = Guarded::build(
        &*cfg.bg,
        &Env {
            monitors: &monitors,
            seed,
            input: cfg.frame.as_ref(),
        },
    );

    // Hear about screen size / monitor changes (to keep covering all of it)
    // and about other windows being mapped or restacked (to stay on top).
    conn.change_window_attributes(
        screen.root,
        &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_NOTIFY),
    )?;
    if conn
        .extension_information(randr::X11_EXTENSION_NAME)?
        .is_some()
    {
        conn.randr_select_input(
            screen.root,
            randr::NotifyMask::SCREEN_CHANGE
                | randr::NotifyMask::CRTC_CHANGE
                | randr::NotifyMask::OUTPUT_CHANGE,
        )?;
    }

    let win = conn.generate_id()?;
    let blank = blank_cursor(&conn, screen.root)?;
    let mask = EventMask::EXPOSURE
        | EventMask::KEY_PRESS
        | EventMask::VISIBILITY_CHANGE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::POINTER_MOTION;
    conn.create_window(
        screen.root_depth,
        win,
        screen.root,
        0,
        0,
        screen.width_in_pixels,
        screen.height_in_pixels,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(screen.black_pixel)
            .event_mask(mask)
            .cursor(blank),
    )?;
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"alock-rs",
    )?;
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"alock-rs\0alock-rs\0",
    )?;
    let bypass = conn
        .intern_atom(false, b"_NET_WM_BYPASS_COMPOSITOR")?
        .reply()?
        .atom;
    conn.change_property32(PropMode::REPLACE, win, bypass, AtomEnum::CARDINAL, &[1])?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))?;

    let presenter = Presenter::new(&conn, win, gc, screen.root_depth, swap, full);
    let mut lk = Locker {
        scene: Canvas::new(full, BLACK),
        out: Canvas::new(full, BLACK),
        conn,
        root: screen.root,
        win,
        full,
        monitors,
        spec: cfg.bg,
        frame: cfg.frame,
        seed,
        theme,
        presenter,
        cursor,
        ptr: (full.w / 2, full.h / 2),
        cursor_drawn: None,
        ui: Ui::new(),
        damage: Vec::new(),
        epoch,
    };
    let t = lk.now();
    lk.theme.draw_all(&mut lk.scene, t, &lk.ui);

    lk.conn.map_window(win)?;
    lk.conn
        .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))?;
    lk.conn.flush()?;
    if let Err(e) = grab(&lk.conn, win, blank) {
        let _ = lk.conn.destroy_window(win);
        let _ = lk.conn.flush();
        return Err(e);
    }
    if let Ok(p) = lk.conn.query_pointer(screen.root)?.reply() {
        lk.ptr = (p.root_x as i32, p.root_y as i32);
    }
    lk.damage.push(full);
    // the layout may have changed between connecting and selecting events
    lk.relayout();
    lk.event_loop(auth, cfg.demo_secs)
}

fn load_cursor(cfg: CursorCfg) -> CursorSet {
    let fallback = || cursor::emblem(48, 0xffcc66, 0x99ccff, false);
    catch_unwind(|| match cfg {
        CursorCfg::Blank => CursorSet::blank(),
        CursorCfg::Image { file, size, hot } => cursor::load_image(&file, size, hot)
            .unwrap_or_else(|e| {
                eprintln!("alock-rs: {e}; using the built-in cursor");
                fallback()
            }),
        CursorCfg::Emblem {
            size,
            star,
            ring,
            pulse,
        } => cursor::emblem(size, star, ring, pulse),
    })
    .unwrap_or_else(|_| {
        eprintln!("alock-rs: loading the cursor panicked; using the built-in cursor");
        fallback()
    })
}

/// The pixel layout we write must be 32 bpp 8-8-8 TrueColor. Returns whether
/// pixels need byte-swapping, or `None` if we cannot draw on this visual; the
/// screen is then locked with a plain black window.
fn check_visual(conn: &RustConnection, screen: &Screen) -> Option<bool> {
    let setup = conn.setup();
    let bpp_ok = setup
        .pixmap_formats
        .iter()
        .any(|f| f.depth == screen.root_depth && f.bits_per_pixel == 32);
    let vis_ok = screen
        .allowed_depths
        .iter()
        .flat_map(|d| &d.visuals)
        .any(|v| {
            v.visual_id == screen.root_visual
                && v.class == VisualClass::TRUE_COLOR
                && v.red_mask == 0xff0000
                && v.green_mask == 0x00ff00
                && v.blue_mask == 0x0000ff
        });
    if !(bpp_ok && vis_ok) {
        eprintln!(
            "alock-rs: unsupported root visual (depth {}); need 24/32-bit TrueColor. \
             Locking with a black screen",
            screen.root_depth
        );
        return None;
    }
    Some(setup.image_byte_order == ImageOrder::MSB_FIRST)
}

fn monitors(conn: &RustConnection, root: Window, full: Rect) -> Vec<Rect> {
    let has = conn
        .extension_information(randr::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();
    if has
        && let Ok(cookie) = conn.randr_get_monitors(root, true)
        && let Ok(reply) = cookie.reply()
    {
        let mut v: Vec<(bool, Rect)> = reply
            .monitors
            .iter()
            .map(|m| {
                (
                    m.primary,
                    Rect::new(m.x as i32, m.y as i32, m.width as i32, m.height as i32)
                        .intersect(&full),
                )
            })
            .filter(|(_, r)| !r.is_empty())
            .collect();
        v.sort_by_key(|(p, r)| (!*p, r.x, r.y));
        if !v.is_empty() {
            return v.into_iter().map(|(_, r)| r).collect();
        }
    }
    vec![full]
}

fn blank_cursor(conn: &RustConnection, root: Window) -> Result<Cursor, Box<dyn Error>> {
    let pix = conn.generate_id()?;
    conn.create_pixmap(1, pix, root, 1, 1)?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, pix, &CreateGCAux::new().foreground(0))?;
    conn.poly_fill_rectangle(
        pix,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        }],
    )?;
    let cur = conn.generate_id()?;
    conn.create_cursor(cur, pix, pix, 0, 0, 0, 0, 0, 0, 0, 0)?;
    conn.free_gc(gc)?;
    conn.free_pixmap(pix)?;
    Ok(cur)
}

/// Retry for ten seconds: the hotkey daemon or a menu may still hold a grab.
/// Without both grabs the screen cannot be locked, so this is the one
/// startup failure that has no fallback.
fn grab(conn: &RustConnection, win: Window, cursor: Cursor) -> Result<(), Box<dyn Error>> {
    let mut kbd = false;
    let mut ptr = false;
    for _ in 0..1000 {
        if !kbd {
            kbd = conn
                .grab_keyboard(
                    false,
                    win,
                    x11rb::CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )?
                .reply()?
                .status
                == GrabStatus::SUCCESS;
        }
        if !ptr {
            let mask =
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
            ptr = conn
                .grab_pointer(
                    false,
                    win,
                    mask,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    win,
                    cursor,
                    x11rb::CURRENT_TIME,
                )?
                .reply()?
                .status
                == GrabStatus::SUCCESS;
        }
        if kbd && ptr {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!(
        "could not grab the {}",
        if kbd { "pointer" } else { "keyboard" }
    )
    .into())
}

fn clock_ms(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

fn merge(rects: &mut Vec<Rect>, full: Rect) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for mut r in rects
        .drain(..)
        .map(|r| r.intersect(&full))
        .filter(|r| !r.is_empty())
    {
        while let Some(i) = out.iter().position(|o| o.overlaps(&r)) {
            r = r.union(&out.swap_remove(i));
        }
        out.push(r);
    }
    if out.iter().map(Rect::area).sum::<i64>() * 10 > full.area() * 6 {
        vec![full]
    } else {
        out
    }
}

impl Locker {
    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    fn cursor_rect(&self, t: u64) -> Option<(Rect, usize)> {
        let s = self.cursor.frame(t)?;
        let idx = if self.cursor.animated() {
            (t / self.cursor.frame_ms.max(1)) as usize % self.cursor.frames.len()
        } else {
            0
        };
        Some((
            Rect::new(self.ptr.0 - s.hot_x, self.ptr.1 - s.hot_y, s.w, s.h),
            idx,
        ))
    }

    /// Compose the damaged areas and send them, unless the previous frame
    /// is still with the server; then the damage waits for the next call.
    fn present(&mut self, t: u64) -> Result<(), Box<dyn Error>> {
        let start = self.now_us();
        if self.presenter.busy(start) {
            return Ok(());
        }
        // the software cursor moved or advanced a frame?
        let now = self.cursor_rect(t);
        if now != self.cursor_drawn {
            if let Some((r, _)) = self.cursor_drawn {
                self.damage.push(r);
            }
            if let Some((r, _)) = now {
                self.damage.push(r);
            }
            self.cursor_drawn = now;
        }
        let rects = merge(&mut self.damage, self.full);
        if rects.is_empty() {
            return Ok(());
        }
        let sprite = self.cursor.frame(t);
        for r in &rects {
            self.out.set_clip(Some(*r));
            self.out.copy_from(&self.scene);
            self.theme.overlay(&mut self.out, t, &self.ui);
            if let (Some(s), Some((cr, _))) = (sprite, now) {
                self.out.sprite(s, cr.x, cr.y);
            }
        }
        self.out.set_clip(None);
        self.presenter.put(&self.conn, &self.out, &rects, start)?;
        let us = self.now_us() - start;
        let st = &mut self.presenter.stats;
        st.compose_us += us;
        st.compose_max_us = st.compose_max_us.max(us);
        Ok(())
    }

    /// Follow a changed screen size or monitor layout: resize the window to
    /// cover the whole screen again and rebuild everything sized to it.
    fn relayout(&mut self) {
        let Some(geo) = self
            .conn
            .get_geometry(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return;
        };
        let full = Rect::new(0, 0, geo.width as i32, geo.height as i32);
        if full.is_empty() {
            return;
        }
        let mons = monitors(&self.conn, self.root, full);
        if full == self.full && mons == self.monitors {
            return;
        }
        let _ = self.conn.configure_window(
            self.win,
            &ConfigureWindowAux::new()
                .x(0)
                .y(0)
                .width(geo.width as u32)
                .height(geo.height as u32)
                .stack_mode(StackMode::ABOVE),
        );
        self.full = full;
        self.monitors = mons;
        self.scene = Canvas::new(full, BLACK);
        self.out = Canvas::new(full, BLACK);
        self.presenter.resize(&self.conn, full);
        self.theme = Guarded::build(
            &*self.spec,
            &Env {
                monitors: &self.monitors,
                seed: self.seed,
                input: self.frame.as_ref(),
            },
        );
        let t = self.now();
        self.theme.draw_all(&mut self.scene, t, &self.ui);
        self.ptr = (
            self.ptr.0.clamp(0, full.w - 1),
            self.ptr.1.clamp(0, full.h - 1),
        );
        self.cursor_drawn = None;
        self.damage = vec![full];
    }

    fn ui_changed(&mut self) {
        let r = self.theme.overlay_rects();
        self.damage.extend(r);
    }

    fn event_loop(
        mut self,
        auth: Authenticator,
        demo_secs: Option<u64>,
    ) -> Result<(), Box<dyn Error>> {
        let mut keymap = Keymap::load(&self.conn)?;
        // Never grows past its capacity, so no unwiped copy is left behind
        // by a reallocation.
        let mut password = Zeroizing::new(String::with_capacity(MAX_PASSWORD + 1));
        auth::lock_memory(password.as_ptr(), password.capacity());
        let (tx, rx) = mpsc::channel::<(u64, AuthEvent)>();
        // The auth helper now running, if any. While `phase` is not
        // `Checking`, it is waiting for an answer to a prompt.
        let mut session: Option<Session> = None;
        let mut generation = 0u64;
        let mut auth_at = 0u64;
        let mut last_key = 0u64;
        let mut message_at = 0u64;
        let has_dpms = self
            .conn
            .extension_information(dpms::X11_EXTENSION_NAME)?
            .is_some();
        let mut screen_off = false;
        let mut dpms_checked = 0u64;
        let mut repaint_at: Vec<u64> = Vec::new();
        let mut relayout = false;
        let mut raise = false;
        let mut raised_at = 0u64;
        let (mut boot, mut mono) = (
            clock_ms(libc::CLOCK_BOOTTIME),
            clock_ms(libc::CLOCK_MONOTONIC),
        );
        let fd = self.conn.stream().as_raw_fd();

        loop {
            // One iteration. Returns true once the screen may be unlocked.
            // A panic anywhere in here is caught below: the locker carries
            // on rather than exit, because exiting unlocks.
            let step = catch_unwind(AssertUnwindSafe(|| -> Result<bool, Box<dyn Error>> {
                let t = self.now();

                // a suspend shows up as boot time advancing while monotonic did not;
                // repaint a few times as the GPU settles after resume
                let (b, m) = (
                    clock_ms(libc::CLOCK_BOOTTIME),
                    clock_ms(libc::CLOCK_MONOTONIC),
                );
                if b.saturating_sub(boot)
                    .saturating_sub(m.saturating_sub(mono))
                    > 2000
                {
                    repaint_at = vec![t + 300, t + 1500, t + 4000];
                    raise = true;
                }
                (boot, mono) = (b, m);
                if repaint_at.first().is_some_and(|&r| t >= r) {
                    repaint_at.remove(0);
                    self.damage.push(self.full);
                }

                // Round trips go before draining events: events that arrive while
                // waiting for a reply are queued inside x11rb and would not wake
                // poll() below.
                if relayout {
                    relayout = false;
                    self.relayout();
                }
                if has_dpms && t.saturating_sub(dpms_checked) >= 2000 {
                    dpms_checked = t;
                    let off = self
                        .conn
                        .dpms_info()
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .is_some_and(|i| i.state && i.power_level != dpms::DPMSMode::ON);
                    if screen_off && !off {
                        self.damage.push(self.full);
                    }
                    screen_off = off;
                }

                while let Some(ev) = self.conn.poll_for_event()? {
                    match ev {
                        Event::ShmCompletion(e) if self.presenter.is_completion(&e) => {
                            let now = self.now_us();
                            self.presenter.completed(now);
                        }
                        Event::Expose(e) => {
                            self.damage.push(Rect::new(
                                e.x as i32,
                                e.y as i32,
                                e.width as i32,
                                e.height as i32,
                            ));
                        }
                        Event::VisibilityNotify(e) if e.state != Visibility::UNOBSCURED => {
                            raise = true;
                        }
                        // the screen was resized or monitors came and went
                        Event::ConfigureNotify(e) if e.window == self.root => relayout = true,
                        Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                            relayout = true;
                        }
                        // another window (a notification, a menu) appeared or was
                        // restacked, possibly above us
                        Event::ConfigureNotify(e) if e.window != self.win => raise = true,
                        Event::MapNotify(e) if e.window != self.win => raise = true,
                        Event::MappingNotify(_) => {
                            // keep the old map if the new one cannot be read
                            if let Ok(k) = Keymap::load(&self.conn) {
                                keymap = k;
                            }
                        }
                        Event::MotionNotify(e) => self.ptr = (e.event_x as i32, e.event_y as i32),
                        Event::ButtonPress(e) if e.detail == 1 => {
                            self.ptr = (e.event_x as i32, e.event_y as i32);
                            self.theme.press(self.ptr.0, self.ptr.1, t);
                        }
                        Event::ButtonRelease(e) if e.detail == 1 => {
                            self.ptr = (e.event_x as i32, e.event_y as i32);
                            self.theme.release(self.ptr.0, self.ptr.1, t);
                        }
                        Event::KeyPress(e) => {
                            let Some(key) = keymap.lookup(e.detail, u16::from(e.state)) else {
                                continue;
                            };
                            if self.ui.phase == Phase::Checking {
                                // Escape gives up on a slow or stuck module
                                if key == Key::Escape {
                                    if let Some(s) = session.take() {
                                        s.cancel();
                                    }
                                    self.ui.phase = Phase::Idle;
                                    self.ui.message = None;
                                    self.ui_changed();
                                }
                                continue;
                            }
                            match key {
                                Key::Char(ch) => {
                                    if password.len() + ch.len_utf8() <= MAX_PASSWORD {
                                        password.push(ch);
                                    }
                                    self.ui.phase = Phase::Typing;
                                }
                                Key::Backspace => {
                                    password.pop();
                                    self.ui.phase = if password.is_empty() {
                                        Phase::Idle
                                    } else {
                                        Phase::Typing
                                    };
                                }
                                Key::Clear => {
                                    password.zeroize();
                                    self.ui.phase = Phase::Idle;
                                }
                                Key::Escape => {
                                    password.zeroize();
                                    self.ui.phase = Phase::Idle;
                                    // also abandons an attempt waiting at a prompt
                                    if let Some(s) = session.take() {
                                        s.cancel();
                                        self.ui.message = None;
                                    }
                                }
                                Key::Enter => {
                                    let sent = if let Some(s) = session.as_mut() {
                                        s.answer(password.as_bytes())
                                    } else {
                                        generation += 1;
                                        self.ui.message = None;
                                        auth.start(generation, password.as_bytes(), tx.clone())
                                            .map(|s| session = Some(s))
                                    };
                                    password.zeroize();
                                    auth_at = t;
                                    match sent {
                                        Ok(()) => self.ui.phase = Phase::Checking,
                                        Err(e) => {
                                            if let Some(s) = session.take() {
                                                s.cancel();
                                            }
                                            self.ui.phase = Phase::Denied;
                                            self.ui.denied_until = t + DENIED_MS;
                                            self.ui.message =
                                                Some(format!("authentication unavailable: {e}"));
                                            message_at = t;
                                        }
                                    }
                                }
                            }
                            self.ui.len = password.chars().count();
                            self.ui.keypresses += 1;
                            last_key = t;
                            self.ui_changed();
                        }
                        _ => {}
                    }
                }

                while let Ok((id, ev)) = rx.try_recv() {
                    // leftovers of a cancelled attempt
                    if session.as_ref().is_none_or(|s| s.id != id) {
                        continue;
                    }
                    match ev {
                        AuthEvent::Done(Ok(())) => {
                            if let Some(s) = session.take() {
                                s.cancel();
                            }
                            self.conn.ungrab_keyboard(x11rb::CURRENT_TIME)?;
                            self.conn.ungrab_pointer(x11rb::CURRENT_TIME)?;
                            self.conn.destroy_window(self.win)?;
                            self.conn.flush()?;
                            return Ok(true);
                        }
                        AuthEvent::Done(Err(_)) => {
                            if let Some(s) = session.take() {
                                s.cancel();
                            }
                            password.zeroize();
                            self.ui.phase = Phase::Denied;
                            self.ui.denied_until = t + DENIED_MS;
                            self.ui.len = 0;
                        }
                        AuthEvent::Message(m) => {
                            self.ui.message = Some(m);
                            message_at = t;
                        }
                        AuthEvent::Prompt(m) => {
                            // the module wants more (a one-time code, a second
                            // password): collect it like the first one
                            self.ui.message = Some(m);
                            message_at = t;
                            self.ui.phase = Phase::Idle;
                            auth_at = t;
                        }
                    }
                    self.ui_changed();
                }

                // timeouts
                if session.is_some() && t.saturating_sub(auth_at) > AUTH_TIMEOUT_MS {
                    if let Some(s) = session.take() {
                        s.cancel();
                    }
                    password.zeroize();
                    self.ui.len = 0;
                    self.ui.phase = Phase::Denied;
                    self.ui.denied_until = t + DENIED_MS;
                    self.ui.message = Some("authentication timed out".into());
                    message_at = t;
                    self.ui_changed();
                }
                if self.ui.phase == Phase::Typing && t.saturating_sub(last_key) > TYPING_TIMEOUT_MS
                {
                    password.zeroize();
                    self.ui.len = 0;
                    self.ui.phase = Phase::Idle;
                    self.ui_changed();
                }
                if self.ui.phase == Phase::Denied && t >= self.ui.denied_until {
                    self.ui.phase = Phase::Idle;
                    self.ui_changed();
                }
                // a prompt stays up for as long as the helper waits for its answer
                if self.ui.message.is_some()
                    && session.is_none()
                    && t.saturating_sub(message_at) > MESSAGE_MS
                {
                    self.ui.message = None;
                    self.ui_changed();
                }
                if demo_secs.is_some_and(|s| t >= s.saturating_mul(1000)) {
                    eprintln!("{}", self.presenter.report(t as f64 / 1000.0));
                    return Ok(true);
                }

                // the earliest moment something needs doing; 250 ms at most so
                // DPMS and resume are noticed
                let mut wake = t + 250;
                if raise && t.saturating_sub(raised_at) >= RAISE_MS {
                    raise = false;
                    raised_at = t;
                    self.conn.configure_window(
                        self.win,
                        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                    )?;
                }
                if raise {
                    wake = wake.min(raised_at + RAISE_MS);
                }
                if relayout {
                    wake = t;
                }
                if !screen_off {
                    if let Some(n) = self
                        .theme
                        .tick(&mut self.scene, t, &self.ui, &mut self.damage)
                    {
                        wake = wake.min(n);
                    }
                    if self.cursor.animated() {
                        let fm = self.cursor.frame_ms.max(1);
                        wake = wake.min((t / fm + 1) * fm);
                    }
                    self.present(t)?;
                }
                match self.ui.phase {
                    // the auth result comes over a channel, not the X socket
                    Phase::Checking => wake = wake.min(t + 50),
                    Phase::Typing => wake = wake.min(last_key + TYPING_TIMEOUT_MS + 1),
                    Phase::Denied => wake = wake.min(self.ui.denied_until),
                    Phase::Idle => {}
                }
                if self.ui.message.is_some() && session.is_none() {
                    wake = wake.min(message_at + MESSAGE_MS + 1);
                }
                if let Some(&r) = repaint_at.first() {
                    wake = wake.min(r);
                }
                self.conn.flush()?;

                let wait = wake.saturating_sub(self.now());
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                unsafe { libc::poll(&mut pfd, 1, wait as i32) };
                Ok(false)
            }));
            match step {
                Ok(Ok(false)) => {}
                Ok(Ok(true)) => return Ok(()),
                // the X connection is gone, and the session with it
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    eprintln!("alock-rs: internal error (panic); still locked");
                    password.zeroize();
                    self.ui.len = 0;
                    if self.ui.phase == Phase::Typing {
                        self.ui.phase = Phase::Idle;
                    }
                    self.scene.set_clip(None);
                    self.out.set_clip(None);
                    self.cursor_drawn = None;
                    self.damage = vec![self.full];
                    // don't spin if it keeps happening
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}
