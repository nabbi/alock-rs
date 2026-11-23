//! Pushing composited pixels to the lock window.
//!
//! With MIT-SHM the frame is written into a shared segment and the server
//! copies it out with `ShmPutImage`. Only one frame is in flight at a time:
//! the next one is composed after the server reports completion. Without
//! this, frames queue up in the socket whenever the server is slower than
//! the theme, and the software cursor trails the real pointer.
//!
//! Without MIT-SHM (remote display, or `ALOCK_RS_NO_SHM=1`) the pixels go
//! through the socket with `PutImage`.
//!
//! On a visual we cannot write pixels for, nothing is sent at all: the lock
//! window keeps its black background and still locks.

use crate::render::{Canvas, Rect};
use std::error::Error;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, Gcontext, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

/// Give up waiting for a completion event after this long.
const STALL_MS: u64 = 250;

struct Segment {
    seg: shm::Seg,
    ptr: *mut u32,
    len: usize,
}

impl Segment {
    fn create(conn: &RustConnection, pixels: usize) -> Result<Segment, Box<dyn Error>> {
        if std::env::var_os("ALOCK_RS_NO_SHM").is_some_and(|v| v != "0") {
            return Err("disabled by ALOCK_RS_NO_SHM".into());
        }
        if conn
            .extension_information(shm::X11_EXTENSION_NAME)?
            .is_none()
        {
            return Err("no MIT-SHM".into());
        }
        conn.shm_query_version()?.reply()?;
        let bytes = pixels * 4;
        // SAFETY: plain SysV shm calls; results are checked before use.
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, bytes, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: id is a fresh segment of `bytes` bytes.
        let ptr = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if ptr as isize == -1 {
            let e = std::io::Error::last_os_error();
            // SAFETY: id is ours.
            unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(e.into());
        }
        let seg = conn.generate_id()?;
        let attached = conn
            .shm_attach(seg, id as u32, true)
            .map_err(Box::<dyn Error>::from)
            .and_then(|c| c.check().map_err(Into::into));
        // The server holds its own attachment now (or failed to); mark the
        // segment for removal so it cannot outlive us.
        // SAFETY: id is ours.
        unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
        if let Err(e) = attached {
            // SAFETY: ptr came from shmat above.
            unsafe { libc::shmdt(ptr) };
            return Err(e);
        }
        Ok(Segment {
            seg,
            ptr: ptr as *mut u32,
            len: pixels,
        })
    }

    fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: ptr maps len u32s (shmat is page aligned) and we only write
        // while no ShmPutImage from it is pending.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: ptr came from shmat.
        unsafe { libc::shmdt(self.ptr as *const libc::c_void) };
    }
}

#[derive(Default)]
pub struct Stats {
    pub frames: u64,
    pub pixels: u64,
    pub compose_us: u64,
    pub compose_max_us: u64,
    pub server_us: u64,
    pub server_max_us: u64,
    pub server_n: u64,
    pub stalls: u64,
}

pub struct Presenter {
    win: Window,
    gc: Gcontext,
    depth: u8,
    /// Byte-swap pixels for the server; `None`: unsupported visual, draw nothing.
    swap: Option<bool>,
    full: Rect,
    shm: Option<Segment>,
    /// When the frame now in flight was sent (µs since start).
    in_flight: Option<u64>,
    pub stats: Stats,
}

impl Presenter {
    pub fn new(
        conn: &RustConnection,
        win: Window,
        gc: Gcontext,
        depth: u8,
        swap: Option<bool>,
        full: Rect,
    ) -> Presenter {
        let shm = Self::segment(conn, swap, full);
        Presenter {
            win,
            gc,
            depth,
            swap,
            full,
            shm,
            in_flight: None,
            stats: Stats::default(),
        }
    }

    fn segment(conn: &RustConnection, swap: Option<bool>, full: Rect) -> Option<Segment> {
        swap?;
        Segment::create(conn, full.area() as usize)
            .inspect_err(|e| eprintln!("alock-rs: MIT-SHM unavailable ({e}), using PutImage"))
            .ok()
    }

    /// The screen changed size: start over with a segment that fits.
    pub fn resize(&mut self, conn: &RustConnection, full: Rect) {
        if let Some(old) = self.shm.take() {
            let _ = conn.shm_detach(old.seg);
        }
        self.full = full;
        self.in_flight = None;
        self.shm = Self::segment(conn, self.swap, full);
    }

    pub fn uses_shm(&self) -> bool {
        self.shm.is_some()
    }

    /// A frame is still being copied by the server; don't touch the buffer.
    /// Returns false (and counts a stall) once it has waited too long.
    pub fn busy(&mut self, now_us: u64) -> bool {
        match self.in_flight {
            Some(t0) if now_us.saturating_sub(t0) > STALL_MS * 1000 => {
                self.stats.stalls += 1;
                self.in_flight = None;
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// The server finished the in-flight `ShmPutImage`.
    pub fn completed(&mut self, now_us: u64) {
        if let Some(t0) = self.in_flight.take() {
            let d = now_us.saturating_sub(t0);
            self.stats.server_us += d;
            self.stats.server_max_us = self.stats.server_max_us.max(d);
            self.stats.server_n += 1;
        }
    }

    pub fn is_completion(&self, e: &shm::CompletionEvent) -> bool {
        e.drawable == self.win && self.shm.as_ref().is_some_and(|s| s.seg == e.shmseg)
    }

    /// Send the given areas of `frame` (a full-screen canvas) to the window.
    pub fn put(
        &mut self,
        conn: &RustConnection,
        frame: &Canvas,
        rects: &[Rect],
        now_us: u64,
    ) -> Result<(), Box<dyn Error>> {
        let Some(swap) = self.swap else {
            return Ok(());
        };
        if rects.is_empty() {
            return Ok(());
        }
        self.stats.frames += 1;
        self.stats.pixels += rects.iter().map(Rect::area).sum::<i64>() as u64;
        let fw = self.full.w as usize;
        let conv = |p: u32| if swap { p.to_be() } else { p.to_le() };
        if let Some(seg) = &mut self.shm {
            let dst = seg.pixels();
            for r in rects {
                for y in r.y..r.bottom() {
                    let o = y as usize * fw + r.x as usize;
                    let src = &frame.px[o..o + r.w as usize];
                    let d = &mut dst[o..o + r.w as usize];
                    if cfg!(target_endian = "little") && !swap {
                        d.copy_from_slice(src);
                    } else {
                        for (d, s) in d.iter_mut().zip(src) {
                            *d = conv(*s);
                        }
                    }
                }
            }
            let seg = seg.seg;
            for (i, r) in rects.iter().enumerate() {
                conn.shm_put_image(
                    self.win,
                    self.gc,
                    self.full.w as u16,
                    self.full.h as u16,
                    r.x as u16,
                    r.y as u16,
                    r.w as u16,
                    r.h as u16,
                    r.x as i16,
                    r.y as i16,
                    self.depth,
                    ImageFormat::Z_PIXMAP.into(),
                    i + 1 == rects.len(),
                    seg,
                    0,
                )?;
            }
            self.in_flight = Some(now_us);
            return Ok(());
        }

        let max_bytes = conn.maximum_request_bytes() - 64;
        for r in rects {
            let w = r.w as usize;
            let max_rows = (max_bytes / (w * 4)).clamp(1, r.h as usize);
            let mut buf = Vec::with_capacity(w * 4 * max_rows);
            let mut y = r.y;
            while y < r.bottom() {
                let n = (max_rows as i32).min(r.bottom() - y);
                buf.clear();
                for yy in y..y + n {
                    let o = yy as usize * fw + r.x as usize;
                    for p in &frame.px[o..o + w] {
                        buf.extend_from_slice(&if swap {
                            p.to_be_bytes()
                        } else {
                            p.to_le_bytes()
                        });
                    }
                }
                conn.put_image(
                    ImageFormat::Z_PIXMAP,
                    self.win,
                    self.gc,
                    r.w as u16,
                    n as u16,
                    r.x as i16,
                    y as i16,
                    0,
                    self.depth,
                    &buf,
                )?;
                y += n;
            }
        }
        Ok(())
    }

    pub fn report(&self, secs: f64) -> String {
        let s = &self.stats;
        let per = |v: u64, n: u64| v as f64 / n.max(1) as f64 / 1000.0;
        let mut out = format!(
            "alock-rs: {}x{} via {}: {:.1} frames/s, {:.0} kpx/frame, compose {:.2} ms avg / {:.2} max",
            self.full.w,
            self.full.h,
            if self.uses_shm() {
                "MIT-SHM"
            } else {
                "PutImage"
            },
            s.frames as f64 / secs.max(0.001),
            s.pixels as f64 / s.frames.max(1) as f64 / 1000.0,
            per(s.compose_us, s.frames),
            s.compose_max_us as f64 / 1000.0,
        );
        if self.uses_shm() {
            out += &format!(
                ", server {:.2} ms avg / {:.2} max, {} stalls",
                per(s.server_us, s.server_n),
                s.server_max_us as f64 / 1000.0,
                s.stalls
            );
        }
        out
    }
}
