//! Keycode -> character translation using the core X keyboard mapping
//! (shift, caps lock, num lock, Mode_switch) plus the way an XKB server lays
//! its first two layouts and the AltGr level out in that mapping: columns
//! 0-1 are layout 1, 2-3 layout 2, 4-5 and 6-7 their AltGr levels. No
//! xkbcommon dependency, so layouts 3-4, dead keys and compose are not
//! resolved.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

#[derive(Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Escape,
    Clear,
}

pub struct Keymap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
    numlock: u16,
    modeswitch: u16,
    level3: u16,
}

const SHIFT: u16 = 1;
const LOCK: u16 = 2;
const CONTROL: u16 = 4;

impl Keymap {
    pub fn load(conn: &impl Connection) -> Result<Self, Box<dyn std::error::Error>> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let km = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
        let mut map = Keymap {
            min,
            per: km.keysyms_per_keycode as usize,
            syms: km.keysyms,
            numlock: 0,
            modeswitch: 0,
            level3: 0,
        };
        let mm = conn.get_modifier_mapping()?.reply()?;
        let per_mod = mm.keycodes_per_modifier() as usize;
        for m in 0..8 {
            let codes = mm.keycodes.get(m * per_mod..(m + 1) * per_mod);
            for &kc in codes.unwrap_or(&[]) {
                for i in 0..map.per {
                    match map.sym(kc, i) {
                        0xff7f => map.numlock = 1 << m,
                        0xff7e => map.modeswitch = 1 << m,
                        0xfe03 => map.level3 = 1 << m,
                        _ => {}
                    }
                }
            }
        }
        Ok(map)
    }

    fn sym(&self, code: u8, i: usize) -> u32 {
        if code < self.min || i >= self.per {
            return 0;
        }
        self.syms
            .get((code - self.min) as usize * self.per + i)
            .copied()
            .unwrap_or(0)
    }

    pub fn lookup(&self, code: u8, state: u16) -> Option<Key> {
        // bits 13-14 of the state carry the active XKB layout
        let group2 = state & self.modeswitch != 0 || state >> 13 & 3 != 0;
        let level3 = self.level3 != 0 && state & self.level3 != 0;
        let col = if level3 { 4 } else { 0 };
        let (mut s0, mut s1) = (0, 0);
        if group2 {
            (s0, s1) = (self.sym(code, col + 2), self.sym(code, col + 3));
        }
        if s0 == 0 && s1 == 0 {
            (s0, s1) = (self.sym(code, col), self.sym(code, col + 1));
        }
        if s1 == 0 {
            s1 = upper(s0);
            s0 = lower(s0);
        }
        let shift = state & SHIFT != 0;
        let lock = state & LOCK != 0;
        let sym = if is_keypad(s1) && state & self.numlock != 0 {
            if shift { s0 } else { s1 }
        } else {
            match (shift, lock) {
                (false, false) => s0,
                (true, false) => s1,
                (false, true) => upper(s0),
                (true, true) => s1,
            }
        };
        let key = match sym {
            0xff0d | 0xff8d => return Some(Key::Enter),
            0xff08 => return Some(Key::Backspace),
            0xff1b => return Some(Key::Escape),
            _ => keysym_char(sym)?,
        };
        if state & CONTROL != 0 {
            return match key.to_ascii_lowercase() {
                'u' | 'c' => Some(Key::Clear),
                'h' => Some(Key::Backspace),
                'j' | 'm' => Some(Key::Enter),
                _ => None,
            };
        }
        Some(Key::Char(key))
    }
}

fn is_keypad(s: u32) -> bool {
    (0xff80..=0xffbd).contains(&s)
}

fn keysym_char(s: u32) -> Option<char> {
    match s {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(s),
        0x0100_0100..=0x0110_ffff => char::from_u32(s - 0x0100_0000),
        0x20ac => Some('€'),
        0xff80 => Some(' '),
        0xffaa => Some('*'),
        0xffab => Some('+'),
        0xffac => Some(','),
        0xffad => Some('-'),
        0xffae => Some('.'),
        0xffaf => Some('/'),
        0xffb0..=0xffb9 => char::from_u32('0' as u32 + s - 0xffb0),
        0xffbd => Some('='),
        _ => None,
    }
}

fn latin1_case(s: u32, up: bool) -> u32 {
    match char::from_u32(s) {
        Some(c) if s < 0x100 => {
            let mut it: Box<dyn Iterator<Item = char>> = if up {
                Box::new(c.to_uppercase())
            } else {
                Box::new(c.to_lowercase())
            };
            match (it.next(), it.next()) {
                (Some(o), None) if (o as u32) < 0x100 => o as u32,
                _ => s,
            }
        }
        _ => s,
    }
}

fn upper(s: u32) -> u32 {
    latin1_case(s, true)
}

fn lower(s: u32) -> u32 {
    latin1_case(s, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Keymap {
        // keycode 10: a/A, 11: 1/!, 12: KP_End/KP_1, 13: Return, 14: e only
        Keymap {
            min: 10,
            per: 2,
            syms: vec![0x61, 0x41, 0x31, 0x21, 0xff9c, 0xffb1, 0xff0d, 0, 0xe9, 0],
            numlock: 16,
            modeswitch: 0,
            level3: 0,
        }
    }

    #[test]
    fn shift_lock_numlock_ctrl() {
        let m = map();
        assert_eq!(m.lookup(10, 0), Some(Key::Char('a')));
        assert_eq!(m.lookup(10, SHIFT), Some(Key::Char('A')));
        assert_eq!(m.lookup(10, LOCK), Some(Key::Char('A')));
        assert_eq!(m.lookup(11, LOCK), Some(Key::Char('1')));
        assert_eq!(m.lookup(11, SHIFT), Some(Key::Char('!')));
        assert_eq!(m.lookup(12, 16), Some(Key::Char('1')));
        assert_eq!(m.lookup(12, 0), None);
        assert_eq!(m.lookup(13, 0), Some(Key::Enter));
        assert_eq!(m.lookup(14, SHIFT), Some(Key::Char('É')));
        assert_eq!(m.lookup(10, CONTROL), None);
        assert_eq!(m.lookup(4, 0), None);
    }

    #[test]
    fn layouts_and_altgr() {
        // keycode 10: q Q | Cyrillic й Й | @ Ω | (nothing); 11: e E, AltGr €
        let m = Keymap {
            min: 10,
            per: 8,
            syms: vec![
                0x71,
                0x51,
                0x0100_0439,
                0x0100_0419,
                0x40,
                0x0100_03a9,
                0,
                0, //
                0x65,
                0x45,
                0,
                0,
                0x20ac,
                0,
                0,
                0,
            ],
            numlock: 16,
            modeswitch: 0,
            level3: 128,
        };
        assert_eq!(m.lookup(10, 0), Some(Key::Char('q')));
        assert_eq!(m.lookup(10, 1 << 13), Some(Key::Char('й')));
        assert_eq!(m.lookup(10, 1 << 13 | SHIFT), Some(Key::Char('Й')));
        assert_eq!(m.lookup(10, 128), Some(Key::Char('@')));
        assert_eq!(m.lookup(10, 128 | SHIFT), Some(Key::Char('Ω')));
        // layout 2 has no AltGr level here: layout 1's is used
        assert_eq!(m.lookup(10, 1 << 13 | 128), Some(Key::Char('@')));
        // a key with one layout types the same in both
        assert_eq!(m.lookup(11, 1 << 13), Some(Key::Char('e')));
        assert_eq!(m.lookup(11, 128), Some(Key::Char('€')));
    }
}
