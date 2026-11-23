# Options

The option syntax matches alock (`-module type:opt=val,flag`):

| option | values |
|---|---|
| `-auth` | `pam[:service=NAME]`; by default the first of `alock-rs`, `system-auth` and `login` found in `/etc/pam.d` |
| `-bg` | `blank[:color=C]` |
| | `image:file=F[,fill\|scale\|center\|tiled][,color=C][,shade=0-99]`; `fill` (crop to cover) is the default; each monitor is laid out separately |
| | `console[:palette=amber\|blue,cycle=SECS,fps=N,title=TEXT]` |
| `-cursor` | `image:file=F[,size=PX][,hotx=N,hoty=N]`: any PNG/JPEG, alpha kept, hotspot defaults to the center |
| | `emblem[:size=PX,color=C,ring=C,pulse]`: built-in compass-star emblem with a translucent glow |
| | `blank` |
| `-input` | `frame[:width=N,input=C,check=C,error=C]` (default) or `none` |
| `-demo SECS` | exits by itself and prints frame timing; for previewing themes, **never for locking** |

Colors are written `#rgb` or `#rrggbb`, or as `black`, `white`, `gray`,
`red`, `green`, `blue`, `yellow` or `orange`. Image files are recognised by
their content, so extensionless symlinks like `~/.images/lock` work.

Keys: Enter submits (an empty Enter is allowed, for fingerprint or U2F
stacks), Backspace / Ctrl-H deletes, Esc / Ctrl-U clears. Typed input is
cleared after 15 s idle. While PAM runs, the screen keeps animating, PAM
info messages such as "Place your finger…" are shown on screen, and Esc
abandons the attempt. If the PAM stack asks for something more after the
password (a one-time code), its prompt is shown and the next line you type
answers it. The account must also pass PAM's account check; an expired
password still unlocks, a locked or expired account does not.
