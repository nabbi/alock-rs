//! Authentication state shared by the renderer and the event loop.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Typing,
    Checking,
    Denied,
}

pub struct Ui {
    /// Number of characters typed (never the characters themselves).
    pub len: usize,
    pub phase: Phase,
    /// Latest PAM info/error text, e.g. "Place your finger on the reader".
    pub message: Option<String>,
    /// Bumped on every accepted key, drives the console flash.
    pub keypresses: u64,
    pub denied_until: u64,
}

impl Ui {
    pub fn new() -> Self {
        Ui {
            len: 0,
            phase: Phase::Idle,
            message: None,
            keypresses: 0,
            denied_until: 0,
        }
    }

    pub fn denied(&self, t: u64) -> bool {
        self.phase == Phase::Denied && t < self.denied_until
    }
}
