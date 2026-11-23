# Limitations

- Keyboard translation uses the core keymap: Shift, Caps Lock, Num Lock,
  Mode_switch, AltGr (ISO_Level3_Shift) and the first two XKB layouts. A
  third or fourth layout, dead keys, compose sequences and symbols outside
  Latin-1 that the server reports as legacy (non-Unicode) keysyms are not
  resolved.
- Other override-redirect windows can be on top for up to a tenth of a
  second before the lock window is raised again.
- Only the default X screen is locked; all of its RandR monitors are covered.
  Zaphod multi-screen setups are not supported.
- The visual must be 24/32-bit TrueColor.
