//! Resolve symbols against the server's current XKB group and key types. Never
//! change the global keyboard map, layout or lock state to synthesize a chord.
use crate::{Error, Result};
use x11rb::{
    protocol::{
        xkb::{self, ConnectionExt as _, GetMapReply},
        xproto::{Keycode, ModMask},
    },
    rust_connection::RustConnection,
};
fn fail(error: impl std::fmt::Display) -> Error {
    Error::action(format!("XKB: {error}"))
}

fn symbol(map: &GetMapReply, key: usize, group: u8, mods: u16) -> Option<u32> {
    let row = map.map.syms_rtrn.as_ref()?.get(key)?;
    let groups = row.group_info & 0x0f;
    if groups == 0 || groups > 4 || row.width == 0 {
        return None;
    }
    let group = if group < groups {
        group
    } else {
        match row.group_info & 0xc0 {
            0x40 => groups - 1,
            0x80 => {
                let redirected = (row.group_info & 0x30) >> 4;
                if redirected < groups { redirected } else { 0 }
            }
            _ => group % groups,
        }
    };
    let kind = map.map.types_rtrn.as_ref()?.get(usize::from(
        row.kt_index[usize::from(group)].checked_sub(map.first_type)?,
    ))?;
    let masked = mods & u16::from(kind.mods_mask);
    let level = kind
        .map
        .iter()
        .find(|entry| entry.active && u16::from(entry.mods_mask) == masked)
        .map_or(0, |entry| entry.level);
    row.syms
        .get(usize::from(group) * usize::from(row.width) + usize::from(level))
        .copied()
        .filter(|symbol| *symbol != 0)
}

pub(super) fn chord(connection: &RustConnection, symbols: &[u32]) -> Result<Vec<Keycode>> {
    let state = connection
        .xkb_get_state(xkb::ID::USE_CORE_KBD.into())
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    let map = connection
        .xkb_get_map(
            xkb::ID::USE_CORE_KBD.into(),
            xkb::MapPart::KEY_TYPES | xkb::MapPart::KEY_SYMS | xkb::MapPart::MODIFIER_MAP,
            0u16.into(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0u16.into(),
            0,
            0,
            0,
            0,
            0,
            0,
        )
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    let rows = map
        .map
        .syms_rtrn
        .as_ref()
        .ok_or_else(|| fail("server omitted key symbols"))?;
    let group = u8::from(state.group);
    let find = |wanted: u32, mods: u16| -> Option<Keycode> {
        (0..rows.len())
            .find(|index| symbol(&map, *index, group, mods) == Some(wanted))
            .and_then(|index| map.first_key_sym.checked_add(u8::try_from(index).ok()?))
    };
    let modifier = |code: Keycode| {
        map.map
            .modmap_rtrn
            .as_ref()
            .and_then(|entries| entries.iter().find(|entry| entry.keycode == code))
            .map_or(0, |entry| u16::from(entry.mods))
    };
    let mut codes = vec![];
    let mut ordinary = vec![];
    let mut explicit_mods = 0;
    for wanted in symbols {
        let base = find(*wanted, 0).or_else(|| find(*wanted, u16::from(state.mods)));
        if let Some(code) = base
            && modifier(code) != 0
            && ![0xffe5, 0xffe6, 0xff7f, 0xff14].contains(wanted)
        {
            if !codes.contains(&code) {
                codes.push(code);
            }
            explicit_mods |= modifier(code);
        } else {
            ordinary.push(*wanted);
        }
    }
    if ordinary.len() > 1
        && ordinary
            .iter()
            .any(|symbol| [0xffe5, 0xffe6, 0xff7f, 0xff14].contains(symbol))
    {
        return Err(Error::invalid(
            "Lock keys cannot be mixed with another key in one chord",
        ));
    }
    // An explicit modifier chord names its base key (Shift+a, Control+v).
    // Preserve exactly those modifiers; CapsLock must not turn Ctrl+v into
    // Ctrl+Shift+v. Bare symbol presses instead honor the live lock state.
    let mut mods = if explicit_mods == 0 {
        // GetState.lookup_mods is the grab/lookup compatibility state, which
        // may omit locks. mods is the effective state delivered in key events.
        u16::from(state.mods)
    } else {
        0
    };
    for wanted in ordinary {
        let found = if let Some(code) = find(wanted, mods) {
            Some((code, false))
        } else {
            find(wanted, mods | u16::from(ModMask::SHIFT)).map(|code| (code, true))
        };
        let (code, shift) = found.ok_or_else(|| {
            Error::unsupported(
                "Requested keysym is unavailable in the effective XKB group and lock state",
            )
        })?;
        if shift && explicit_mods & u16::from(ModMask::SHIFT) == 0 {
            let shift = find(0xffe1, 0).ok_or_else(|| fail("Shift key is not mapped"))?;
            if !codes.contains(&shift) {
                codes.push(shift);
            }
            mods |= u16::from(ModMask::SHIFT);
        }
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    Ok(codes)
}
