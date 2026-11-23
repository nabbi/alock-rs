//! Dev helper: inject input through XTEST so the locker can be exercised in
//! Xvfb. Usage: xdrive move X Y | click X Y | hold X Y MS | type TEXT | key KEYSYM_HEX
use std::{thread::sleep, time::Duration};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (conn, sn) = x11rb::connect(None)?;
    let root = conn.setup().roots[sn].root;
    let mv = |x: i16, y: i16| {
        conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)
            .map(|_| ())
    };
    let btn = |ev: u8| conn.xtest_fake_input(ev, 1, 0, root, 0, 0, 0).map(|_| ());
    let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
    let km = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = km.keysyms_per_keycode as usize;
    let find = |sym: u32| -> Option<(u8, bool)> {
        km.keysyms
            .iter()
            .position(|&s| s == sym)
            .map(|i| (min + (i / per) as u8, i % per == 1))
    };
    let key = |sym: u32| -> Result<(), Box<dyn std::error::Error>> {
        let (code, shift) = find(sym).ok_or("no keycode")?;
        let shift_code = find(0xffe1).unwrap().0;
        if shift {
            conn.xtest_fake_input(KEY_PRESS_EVENT, shift_code, 0, root, 0, 0, 0)?;
        }
        conn.xtest_fake_input(KEY_PRESS_EVENT, code, 0, root, 0, 0, 0)?;
        conn.xtest_fake_input(KEY_RELEASE_EVENT, code, 0, root, 0, 0, 0)?;
        if shift {
            conn.xtest_fake_input(KEY_RELEASE_EVENT, shift_code, 0, root, 0, 0, 0)?;
        }
        conn.flush()?;
        sleep(Duration::from_millis(15));
        Ok(())
    };
    match a[0].as_str() {
        "move" => mv(a[1].parse()?, a[2].parse()?)?,
        "click" | "hold" => {
            mv(a[1].parse()?, a[2].parse()?)?;
            conn.flush()?;
            sleep(Duration::from_millis(30));
            btn(BUTTON_PRESS_EVENT)?;
            conn.flush()?;
            let ms = if a[0] == "hold" { a[3].parse()? } else { 60 };
            sleep(Duration::from_millis(ms));
            btn(BUTTON_RELEASE_EVENT)?;
        }
        "type" => {
            for ch in a[1].chars() {
                key(ch as u32)?;
            }
        }
        "key" => key(u32::from_str_radix(&a[1], 16)?)?,
        _ => return Err("unknown command".into()),
    }
    conn.flush()?;
    conn.get_input_focus()?.reply()?;
    Ok(())
}
