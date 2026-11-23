# Coming from alock

## Dropped from alock

These alock features were not carried over:

- `-auth passwd`, `-auth hash` and `-auth none`: setuid/crypt paths. PAM
  covers these.
- `-bg none` and `-bg shade`: they read or expose the live desktop.
- `-cursor none`, `glyph` and `xcursor`: they used the hardware cursor plane.
  The `image` cursor covers custom cursors.
- xbacklight, X resources (`ALock.*`), imlib2 and the autotools build.
