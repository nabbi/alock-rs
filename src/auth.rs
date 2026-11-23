//! PAM authentication in a helper process.
//!
//! The locker never loads PAM itself. For every attempt it re-executes its
//! own binary as `alock-rs --auth-helper ...` and talks to it over pipes, so
//! a crashing or hanging PAM module (fingerprint, U2F, network auth) cannot
//! take the lock window down with it: a dead helper is a failed attempt, and
//! a stuck one is killed on Escape or after a timeout.
//!
//! Wire format, helper -> locker: `[type u8][len u16 LE][payload]` with
//! type `M` (info/error text), `P` (the module asks for more input),
//! `S` (success) or `F` (failure text). Locker -> helper: `[len u16 LE]
//! [bytes]`, first the typed password, then one frame per `P`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::ptr;
use std::sync::mpsc::Sender;
use zeroize::Zeroizing;

pub const HELPER_FLAG: &str = "--auth-helper";

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_NEW_AUTHTOK_REQD: c_int = 12;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_REFRESH_CRED: c_int = 0x0010;
const PAM_MAX_NUM_MSG: c_int = 32;

/// Service names tried, in order, when `-auth pam` names none.
const SERVICES: [&str; 3] = ["alock-rs", "system-auth", "login"];

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(
    c_int,
    *mut *const PamMessage,
    *mut *mut PamResponse,
    *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

#[repr(C)]
struct PamHandle {
    _p: [u8; 0],
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_acct_mgmt(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_setcred(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut PamHandle, errnum: c_int) -> *const c_char;
}

/// Keep secrets out of core dumps and away from ptrace by other processes
/// of the same user. Best effort.
pub fn harden() {
    let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscalls with valid arguments.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &none);
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

/// Keep a secret buffer out of swap. Best effort.
pub fn lock_memory(p: *const u8, len: usize) {
    if len > 0 {
        // SAFETY: the range belongs to a live allocation of the caller.
        unsafe { libc::mlock(p as *const c_void, len) };
    }
}

// ---------------------------------------------------------------- locker side

pub enum AuthEvent {
    Message(String),
    /// The PAM stack wants another answer (e.g. a one-time code).
    Prompt(String),
    Done(Result<(), String>),
}

#[derive(Clone)]
pub enum Authenticator {
    Pam {
        service: String,
    },
    /// `test-auth` builds only: compare against a fixed string.
    #[cfg(feature = "test-auth")]
    Test(String),
}

impl Authenticator {
    pub fn pam(service: Option<&str>) -> Result<Self, String> {
        let service = match service {
            Some(s) => s.to_string(),
            None => SERVICES
                .iter()
                .find(|s| {
                    ["/etc/pam.d", "/usr/lib/pam.d"]
                        .iter()
                        .any(|d| std::path::Path::new(d).join(s).exists())
                })
                .unwrap_or(&SERVICES[1])
                .to_string(),
        };
        if service.is_empty() || service.contains(['/', '\0']) {
            return Err("bad PAM service name".into());
        }
        current_user()?;
        Ok(Authenticator::Pam { service })
    }

    /// Start one authentication attempt. Events are tagged with `id`.
    pub fn start(
        &self,
        id: u64,
        password: &[u8],
        tx: Sender<(u64, AuthEvent)>,
    ) -> io::Result<Session> {
        // /proc/self/exe keeps working when the binary on disk was replaced
        // by an upgrade while the screen is locked.
        let mut cmd = Command::new("/proc/self/exe");
        cmd.arg0("alock-rs").arg(HELPER_FLAG);
        match self {
            Authenticator::Pam { service } => cmd.arg("pam").arg(service),
            #[cfg(feature = "test-auth")]
            Authenticator::Test(want) => cmd.arg("test").arg(want),
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        std::thread::spawn(move || {
            let done = loop {
                match read_frame(&mut stdout) {
                    Ok((b'M', text)) => {
                        let _ = tx.send((id, AuthEvent::Message(lossy(&text))));
                    }
                    Ok((b'P', text)) => {
                        let _ = tx.send((id, AuthEvent::Prompt(lossy(&text))));
                    }
                    Ok((b'S', _)) => break Ok(()),
                    Ok((b'F', text)) => break Err(lossy(&text)),
                    Ok(_) => break Err("bad reply from the auth helper".into()),
                    Err(_) => break Err("the auth helper died".into()),
                }
            };
            let _ = tx.send((id, AuthEvent::Done(done)));
        });
        let mut s = Session { child, stdin, id };
        if let Err(e) = s.answer(password) {
            s.cancel();
            return Err(e);
        }
        Ok(s)
    }
}

/// A running helper.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    pub id: u64,
}

impl Session {
    /// Send the password, or the answer to a [`AuthEvent::Prompt`].
    pub fn answer(&mut self, secret: &[u8]) -> io::Result<()> {
        let n = secret.len().min(u16::MAX as usize);
        self.stdin.write_all(&(n as u16).to_le_bytes())?;
        self.stdin.write_all(&secret[..n])
    }

    /// Kill the helper (if it still runs) and reap it.
    pub fn cancel(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 3];
    r.read_exact(&mut head)?;
    let mut body = vec![0u8; u16::from_le_bytes([head[1], head[2]]) as usize];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

fn write_frame(w: &mut impl Write, ty: u8, body: &[u8]) -> io::Result<()> {
    let n = body.len().min(u16::MAX as usize);
    let mut buf = Vec::with_capacity(3 + n);
    buf.push(ty);
    buf.extend_from_slice(&(n as u16).to_le_bytes());
    buf.extend_from_slice(&body[..n]);
    w.write_all(&buf)
}

fn current_user() -> Result<CString, String> {
    // SAFETY: getpwuid returns a pointer into static storage; we copy the
    // name out immediately. Both callers are single-threaded at this point.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return Err("cannot look up the current user".into());
        }
        Ok(CStr::from_ptr((*pw).pw_name).to_owned())
    }
}

// ---------------------------------------------------------------- helper side

/// Unbuffered pipe ends, so no secret lingers in a stdio buffer.
struct Pipes {
    rx: ManuallyDrop<File>,
    tx: ManuallyDrop<File>,
}

impl Pipes {
    fn secret(&mut self) -> io::Result<Zeroizing<Vec<u8>>> {
        let mut head = [0u8; 2];
        self.rx.read_exact(&mut head)?;
        let n = u16::from_le_bytes(head) as usize;
        // sized once: growing would leave unwiped copies behind
        let mut buf = Zeroizing::new(vec![0u8; n]);
        lock_memory(buf.as_ptr(), n);
        self.rx.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Entry point of `alock-rs --auth-helper KIND VALUE`.
pub fn helper_main(args: &[String]) -> ExitCode {
    harden();
    // SAFETY: fds 0-2 are ours; PAM modules that print to stdout must not
    // corrupt the protocol, so the pipe moves to a private fd and fd 1
    // becomes stderr.
    let mut io = unsafe {
        let out = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
        if out < 0 {
            return ExitCode::FAILURE;
        }
        libc::dup2(2, 1);
        Pipes {
            rx: ManuallyDrop::new(File::from_raw_fd(0)),
            tx: ManuallyDrop::new(File::from_raw_fd(out)),
        }
    };
    let result = match io.secret() {
        Err(_) => return ExitCode::FAILURE,
        Ok(first) => match args {
            [kind, service] if kind == "pam" => pam_auth(service, first, &mut io),
            #[cfg(feature = "test-auth")]
            [kind, want] if kind == "test" => {
                std::thread::sleep(std::time::Duration::from_millis(600));
                if first.as_slice() == want.as_bytes() {
                    Ok(())
                } else {
                    Err("wrong password".into())
                }
            }
            _ => Err("bad auth helper arguments".into()),
        },
    };
    let sent = match &result {
        Ok(()) => write_frame(&mut *io.tx, b'S', b""),
        Err(e) => write_frame(&mut *io.tx, b'F', e.as_bytes()),
    };
    if result.is_ok() && sent.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct ConvData<'a> {
    /// What was typed before Enter; answers the first prompt.
    first: Option<Zeroizing<Vec<u8>>>,
    io: &'a mut Pipes,
}

unsafe extern "C" fn conversation(
    n: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if n <= 0 || n > PAM_MAX_NUM_MSG || msg.is_null() || resp.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    // SAFETY: PAM hands back the appdata pointer we gave pam_start, and the
    // message array it promises holds `n` entries (Linux-PAM layout). PAM
    // calls this from the thread running pam_authenticate only.
    unsafe {
        let data = &mut *(appdata as *mut ConvData);
        let out = libc::calloc(n as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        if out.is_null() {
            return PAM_BUF_ERR;
        }
        for i in 0..n as usize {
            let m = &**msg.add(i);
            let text = if m.msg.is_null() {
                &b""[..]
            } else {
                CStr::from_ptr(m.msg).to_bytes()
            };
            match m.msg_style {
                PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                    let secret = match data.first.take() {
                        Some(s) => Ok(s),
                        None => write_frame(&mut *data.io.tx, b'P', text)
                            .and_then(|()| data.io.secret()),
                    };
                    let Ok(secret) = secret.as_deref().map(|s| &s[..]) else {
                        free_responses(out, n as usize);
                        return PAM_CONV_ERR;
                    };
                    if secret.contains(&0) {
                        free_responses(out, n as usize);
                        return PAM_CONV_ERR;
                    }
                    let p = libc::malloc(secret.len() + 1) as *mut u8;
                    if p.is_null() {
                        free_responses(out, n as usize);
                        return PAM_BUF_ERR;
                    }
                    lock_memory(p, secret.len() + 1);
                    ptr::copy_nonoverlapping(secret.as_ptr(), p, secret.len());
                    *p.add(secret.len()) = 0;
                    (*out.add(i)).resp = p as *mut c_char;
                }
                PAM_ERROR_MSG | PAM_TEXT_INFO => {
                    let _ = write_frame(&mut *data.io.tx, b'M', text);
                }
                _ => {
                    free_responses(out, n as usize);
                    return PAM_CONV_ERR;
                }
            }
        }
        *resp = out;
    }
    PAM_SUCCESS
}

unsafe fn free_responses(out: *mut PamResponse, n: usize) {
    unsafe {
        for i in 0..n {
            let r = (*out.add(i)).resp;
            if !r.is_null() {
                let len = libc::strlen(r);
                ptr::write_bytes(r, 0, len);
                libc::free(r as *mut c_void);
            }
        }
        libc::free(out as *mut c_void);
    }
}

fn pam_auth(service: &str, first: Zeroizing<Vec<u8>>, io: &mut Pipes) -> Result<(), String> {
    let service = CString::new(service).map_err(|_| "bad PAM service name")?;
    let user = current_user()?;
    let mut data = ConvData {
        first: Some(first),
        io,
    };
    let conv = PamConv {
        conv: conversation,
        appdata_ptr: &mut data as *mut ConvData as *mut c_void,
    };
    let mut h: *mut PamHandle = ptr::null_mut();
    // SAFETY: all pointers outlive the handle; pam_end is always called.
    unsafe {
        let rc = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut h);
        if rc != PAM_SUCCESS || h.is_null() {
            return Err(format!("pam_start failed ({rc})"));
        }
        let mut rc = pam_authenticate(h, 0);
        if rc == PAM_SUCCESS {
            // A locked or expired account must not unlock. An expired
            // password may: it gets changed at the next login, and refusing
            // here would only trap the session.
            rc = match pam_acct_mgmt(h, 0) {
                PAM_NEW_AUTHTOK_REQD => PAM_SUCCESS,
                rc => rc,
            };
        }
        let result = if rc == PAM_SUCCESS {
            // refresh e.g. kerberos tickets; failure here must not keep us locked
            let _ = pam_setcred(h, PAM_REFRESH_CRED);
            Ok(())
        } else {
            let s = pam_strerror(h, rc);
            Err(if s.is_null() {
                format!("PAM error {rc}")
            } else {
                CStr::from_ptr(s).to_string_lossy().into_owned()
            })
        };
        pam_end(h, rc);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b'M', b"Place your finger").unwrap();
        write_frame(&mut buf, b'S', b"").unwrap();
        let mut r = &buf[..];
        assert_eq!(
            read_frame(&mut r).unwrap(),
            (b'M', b"Place your finger".to_vec())
        );
        assert_eq!(read_frame(&mut r).unwrap(), (b'S', Vec::new()));
        assert!(read_frame(&mut r).is_err(), "EOF is an error, not success");
    }

    #[test]
    fn service_names() {
        assert!(Authenticator::pam(Some("../etc/x")).is_err());
        assert!(Authenticator::pam(Some("")).is_err());
    }
}
