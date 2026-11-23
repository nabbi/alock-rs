# Design

## Why a rewrite: the NVIDIA bleed-through

With alock, after resume NVIDIA showed stale desktop pixels through the
transparent parts of the ARGB cursor. Fifteen patch iterations (`v1`–`v15`,
arkq/alock#24) showed that no X11 API can wait for the driver's VRAM
restore. The only reliable fix is to keep alpha off the hardware cursor
plane entirely.

alock-rs is built that way from the start:

- The pointer is grabbed with a fully transparent 1×1 cursor, so the
  hardware cursor plane never holds alpha pixels.
- The cursor image is composited **in software**, together with the
  background and the UI, into a single override-redirect window. Only
  damaged rectangles are pushed, through MIT-SHM (`ShmPutImage`) with one
  frame in flight at a time, so frames never queue up behind a slow server.
  Without MIT-SHM (e.g. a remote display) it falls back to `PutImage`.
- If a resume is detected (`CLOCK_BOOTTIME` jumps ahead of
  `CLOCK_MONOTONIC`), the screen is fully repainted at +0.3 s, +1.5 s and
  +4 s, and the window is raised again.
- `_NET_WM_BYPASS_COMPOSITOR` is set so compositors leave the window alone.
- When DPMS turns the monitors off, rendering pauses.

## Staying locked

For a locker, exiting is unlocking, so after the screen is locked the only
ways out are a successful authentication and losing the X server:

- A background or cursor image that is missing, corrupt or oversized
  (more than 16384 px a side) is replaced by a blank screen or the built-in
  cursor, with a warning on stderr. So is an unsupported visual: the screen
  is locked black. The one startup failure without a fallback is not
  getting the keyboard and pointer grabs within ten seconds.
- A panic in a theme switches to a blank screen; a panic anywhere else in
  the event loop is caught and the loop carries on.
- PAM runs in a helper process (`alock-rs --auth-helper`, started for each
  attempt). A crashing module is a failed attempt, and a stuck one is
  killed with Esc or after two minutes.
- When the screen is resized or a monitor is plugged in, the window, the
  theme and the buffers are rebuilt to cover the new layout.
- When another window is mapped or restacked (a notification, say), the
  lock window is raised above it again.
- Core dumps and ptrace by other processes are disabled, and the password
  buffers are locked out of swap and wiped after use.

## Themes

Each `-bg` type is a theme in `src/theme/`: `plain.rs` (`blank`, `image`)
and `console.rs`. A theme implements the `Theme` trait in
`src/theme/mod.rs`: draw the scene, advance its animation and report the
damaged areas, draw the auth feedback, and optionally react to clicks.
Themes never see key presses. To add one, write `src/theme/NAME.rs` and add
an entry to `REGISTRY`; option parsing and `-help` pick it up from there.

## Scripted checks

`examples/xdrive.rs` is a development helper that sends input through
XTEST, for scripted checks under `Xvfb`. For those runs,
`cargo build --features test-auth` accepts `-auth test:password=X`; a
normal build does not, debug or release.
