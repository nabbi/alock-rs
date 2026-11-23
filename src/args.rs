//! alock-compatible command line: `-module type:opt=val,opt,...`.

use crate::theme::{self, need, num};

pub enum AuthCfg {
    Pam {
        /// `None`: the first of alock-rs, system-auth, login that exists.
        service: Option<String>,
    },
    #[cfg(feature = "test-auth")]
    Test(String),
}

pub enum CursorCfg {
    Blank,
    Image {
        file: String,
        size: Option<u32>,
        hot: (Option<i32>, Option<i32>),
    },
    Emblem {
        size: u32,
        star: u32,
        ring: u32,
        pulse: bool,
    },
}

#[derive(Clone, Copy)]
pub struct FrameCfg {
    pub width: i32,
    pub input: u32,
    pub check: u32,
    pub error: u32,
}

pub struct Config {
    pub auth: AuthCfg,
    pub bg: Box<dyn theme::Spec>,
    pub cursor: CursorCfg,
    pub frame: Option<FrameCfg>,
    pub demo_secs: Option<u64>,
}

pub enum Parsed {
    Run(Config),
    Exit(String),
}

pub fn usage() -> String {
    let bg: String = theme::REGISTRY
        .iter()
        .map(|e| format!("  -bg {}\n", e.usage))
        .collect();
    format!(
        "\
alock-rs - X11 screen locker

usage: alock-rs [-auth TYPE] [-bg TYPE] [-cursor TYPE] [-input TYPE] [-demo SECS]

  -auth pam[:service=NAME]          PAM service (default: the first of
                                    alock-rs, system-auth, login that exists)
{bg}  -cursor blank
  -cursor image:file=F[,size=PX][,hotx=N,hoty=N]
  -cursor emblem[:size=PX,color=C,ring=C,pulse]
  -input frame[:width=N,input=C,check=C,error=C] | none
  -demo SECS    exit by itself after SECS and print frame timing
                (theme preview; never use for locking)
  -h, -help     -v, -version

colors: #rgb, #rrggbb, or black white gray red green blue yellow orange
console: click blocks/bars to flip them, CONDITION cycles green/yellow/red,
         tabs switch screens, long press anywhere toggles red alert"
    )
}

pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut cfg = Config {
        auth: AuthCfg::Pam { service: None },
        bg: Box::new(theme::plain::Backdrop::Blank { color: 0 }),
        cursor: CursorCfg::Emblem {
            size: 48,
            star: 0xffcc66,
            ring: 0x99ccff,
            pulse: false,
        },
        frame: Some(FrameCfg {
            width: 8,
            input: 0x3399ff,
            check: 0xffcc00,
            error: 0xff3333,
        }),
        demo_secs: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let flag = a
            .strip_prefix("--")
            .or_else(|| a.strip_prefix('-'))
            .ok_or(format!("unexpected argument: {a}"))?;
        let mut val = || it.next().cloned().ok_or(format!("-{flag} needs a value"));
        match flag {
            "h" | "help" => return Ok(Parsed::Exit(usage())),
            "v" | "version" => {
                return Ok(Parsed::Exit(format!(
                    "alock-rs {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }
            "a" | "auth" => cfg.auth = parse_auth(&val()?)?,
            "b" | "bg" => cfg.bg = parse_bg(&val()?)?,
            "c" | "cursor" => cfg.cursor = parse_cursor(&val()?)?,
            "i" | "input" => cfg.frame = parse_input(&val()?)?,
            "demo" => cfg.demo_secs = Some(val()?.parse().map_err(|_| "-demo needs seconds")?),
            _ => return Err(format!("unknown option -{flag} (try -help)")),
        }
    }
    Ok(Parsed::Run(cfg))
}

/// Split `type:k=v,flag,...` into the type and its options.
fn module(spec: &str) -> (&str, Vec<(&str, Option<&str>)>) {
    let (name, rest) = spec.split_once(':').unwrap_or((spec, ""));
    let opts = rest
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|o| match o.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (o, None),
        })
        .collect();
    (name, opts)
}

pub fn parse_color(s: &str) -> Result<u32, String> {
    let named = match s.to_ascii_lowercase().as_str() {
        "black" => Some(0x000000),
        "white" => Some(0xffffff),
        "gray" | "grey" => Some(0x808080),
        "red" => Some(0xff0000),
        "green" => Some(0x00ff00),
        "blue" => Some(0x0000ff),
        "yellow" => Some(0xffff00),
        "orange" => Some(0xffa500),
        _ => None,
    };
    if let Some(c) = named {
        return Ok(c);
    }
    let hex = s.strip_prefix('#').ok_or(format!("bad color {s}"))?;
    let v = u32::from_str_radix(hex, 16).map_err(|_| format!("bad color {s}"))?;
    match hex.len() {
        6 => Ok(v),
        3 => Ok(((v >> 8 & 0xf) * 0x11) << 16 | ((v >> 4 & 0xf) * 0x11) << 8 | ((v & 0xf) * 0x11)),
        _ => Err(format!("bad color {s}")),
    }
}

fn parse_auth(spec: &str) -> Result<AuthCfg, String> {
    let (name, opts) = module(spec);
    match name {
        "pam" => {
            let mut service = None;
            for (k, v) in opts {
                match k {
                    "service" => service = Some(need(k, v)?.to_string()),
                    _ => return Err(format!("-auth pam: unknown option {k}")),
                }
            }
            Ok(AuthCfg::Pam { service })
        }
        #[cfg(feature = "test-auth")]
        "test" => {
            let pw = opts
                .iter()
                .find(|(k, _)| *k == "password")
                .and_then(|(_, v)| *v)
                .unwrap_or("test");
            Ok(AuthCfg::Test(pw.into()))
        }
        _ => Err(format!("-auth: unknown type {name} (pam)")),
    }
}

fn parse_bg(spec: &str) -> Result<Box<dyn theme::Spec>, String> {
    let (name, opts) = module(spec);
    theme::parse(name, &opts)
}

fn parse_cursor(spec: &str) -> Result<CursorCfg, String> {
    let (name, opts) = module(spec);
    match name {
        "blank" => Ok(CursorCfg::Blank),
        "image" => {
            let (mut file, mut size, mut hx, mut hy) = (None, None, None, None);
            for (k, v) in opts {
                match k {
                    "file" => file = Some(need(k, v)?.to_string()),
                    "size" => size = Some(num::<u32>(k, v)?.clamp(8, 1024)),
                    "hotx" => hx = Some(num(k, v)?),
                    "hoty" => hy = Some(num(k, v)?),
                    _ => return Err(format!("-cursor image: unknown option {k}")),
                }
            }
            Ok(CursorCfg::Image {
                file: file.ok_or("-cursor image needs file=")?,
                size,
                hot: (hx, hy),
            })
        }
        "emblem" => {
            let (mut size, mut star, mut ring, mut pulse) = (48, 0xffcc66, 0x99ccff, false);
            for (k, v) in opts {
                match k {
                    "size" => size = num::<u32>(k, v)?.clamp(16, 512),
                    "color" => star = parse_color(need(k, v)?)?,
                    "ring" => ring = parse_color(need(k, v)?)?,
                    "pulse" => pulse = true,
                    _ => return Err(format!("-cursor emblem: unknown option {k}")),
                }
            }
            Ok(CursorCfg::Emblem {
                size,
                star,
                ring,
                pulse,
            })
        }
        _ => Err(format!(
            "-cursor: unknown type {name} (blank, image, emblem)"
        )),
    }
}

fn parse_input(spec: &str) -> Result<Option<FrameCfg>, String> {
    let (name, opts) = module(spec);
    match name {
        "none" => Ok(None),
        "frame" => {
            let mut f = FrameCfg {
                width: 8,
                input: 0x3399ff,
                check: 0xffcc00,
                error: 0xff3333,
            };
            for (k, v) in opts {
                match k {
                    "width" => f.width = num::<i32>(k, v)?.clamp(1, 200),
                    "input" => f.input = parse_color(need(k, v)?)?,
                    "check" => f.check = parse_color(need(k, v)?)?,
                    "error" => f.error = parse_color(need(k, v)?)?,
                    _ => return Err(format!("-input frame: unknown option {k}")),
                }
            }
            Ok(Some(f))
        }
        _ => Err(format!("-input: unknown type {name} (frame, none)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(a: &[&str]) -> Config {
        match parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap() {
            Parsed::Run(c) => c,
            Parsed::Exit(_) => panic!(),
        }
    }

    #[test]
    fn alock_compatible_command_line() {
        let c = run(&[
            "-auth",
            "pam",
            "-bg",
            "image:file=/x/lock,shade=30",
            "-cursor",
            "image:file=/x/larry.png",
        ]);
        assert!(matches!(c.auth, AuthCfg::Pam { service: None }));
        assert!(matches!(c.cursor, CursorCfg::Image { ref file, .. } if file == "/x/larry.png"));
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fc0").unwrap(), 0xffcc00);
        assert_eq!(parse_color("#123456").unwrap(), 0x123456);
        assert!(parse_color("123").is_err());
    }

    #[test]
    fn rejects_unknown() {
        assert!(parse(&["-bg".into(), "shade".into()]).is_err());
        assert!(parse(&["-bg".into(), "image".into()]).is_err());
        assert!(parse(&["-bg".into(), "console:palette=pink".into()]).is_err());
        assert!(parse(&["-bg".into(), "console:fps=30,palette=blue".into()]).is_ok());
        assert!(parse(&["-auth".into(), "passwd".into()]).is_err());
    }
}
