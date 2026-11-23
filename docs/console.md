# Console theme

The console is a frame of rounded elbows, colour blocks and segmented bars.
Next to it are scrolling data grids, a clock, an auth panel and four screens
that cycle: **sensor sweep** (radar), **harmonic analysis**, **power
distribution** and **stellar grid**.

Touching the console only changes what is drawn. It has no effect on
authentication.

| action | effect |
|---|---|
| click sidebar block | flips it lit/dim with a new code |
| click bar segment / elbow | cycles its colour |
| click CONDITION | GREEN → YELLOW → RED |
| click tab `01`–`04` | switches screen (auto-cycle pauses for a while) |
| click inside a screen | sends a ping; also adds a radar contact, a tracked target, a power surge or a waveform burst, depending on the screen |
| click a data row | pins its highlight |
| **long press anywhere** (0.7 s) | toggles **red alert**: the whole frame flashes red |
| typing | blocks flash on each key |
| wrong password | brief red alert and ACCESS DENIED |

Everything is original and drawn in code: shapes, palette, the 5×7 bitmap
font, the labels and the compass-star emblem. No franchise names, fonts,
logos, insignia or artwork are used, so the theme is free to share.
The image background is idle unless the cursor moves.

The console runs at 30 fps by default (`fps=N`, 1–60). `-demo` reports
the achieved frame rate, the time spent composing each frame and how long
the X server took to take it, which is the place to start if it feels slow.
`ALOCK_RS_NO_SHM=1` forces the `PutImage` path for comparison.
