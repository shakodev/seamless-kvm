//! Physical IBM set-1 scan positions to HIToolbox virtual key codes. Text is
//! interpreted by the selected macOS keyboard layout; no keystroke logging.
pub const SHIFT: u64 = 1 << 17;
pub const CONTROL: u64 = 1 << 18;
pub const OPTION: u64 = 1 << 19;
pub const COMMAND: u64 = 1 << 20;
pub const CAPS: u64 = 1 << 16;
pub const LEFT_COMMAND: u64 = COMMAND | 0x08;
pub const RIGHT_SHIFT: u64 = SHIFT | 0x04;

pub fn modifier(code: u16) -> u64 {
    match code {
        // Device-side flags preserve independent left/right transitions, even
        // when the other side remains down (IOKit/hidsystem/IOLLEvent.h).
        56 => SHIFT | 0x02,
        60 => RIGHT_SHIFT,
        59 => CONTROL | 0x01,
        62 => CONTROL | 0x2000,
        58 => OPTION | 0x20,
        61 => OPTION | 0x40,
        54 => COMMAND | 0x10,
        55 => LEFT_COMMAND,
        _ => 0,
    }
}

pub fn event_flags(code: u16, modifiers: u64) -> u64 {
    // NSEvent's numeric-pad flag also identifies arrow keys. Do not carry it
    // onto unrelated keyboard or mouse events.
    if matches!(code, 65 | 67 | 69 | 71 | 75 | 76 | 78 | 81..=89 | 91 | 92 | 123..=126) {
        modifiers | (1 << 21)
    } else {
        modifiers
    }
}

pub fn translate(vk: u16, scan: u16, extended: bool, win_to_command: bool) -> Option<u16> {
    // Pause shares scan 0x45 with NumLock; PrintScreen can include fake Shift.
    // Quartz has no dedicated PrintScreen/Pause: use the Mac F13/F15 positions.
    match vk {
        0x13 => return Some(113),
        0x2c => return Some(105),
        0x5b => return win_to_command.then_some(55),
        0x5c => return win_to_command.then_some(54),
        0x7c..=0x83 => return Some([105, 107, 113, 106, 64, 79, 80, 90][usize::from(vk - 0x7c)]), // F13..F20
        _ => {}
    }
    if extended {
        return Some(match scan {
            0x1c => 76, // keypad Enter
            0x1d => 62, // right Control
            0x35 => 75, // keypad divide
            0x38 => 61, // right Option (AltGr is host Ctrl+Alt)
            0x47 => 115,
            0x48 => 126,
            0x49 => 116,
            0x4b => 123,
            0x4d => 124,
            0x4f => 119,
            0x50 => 125,
            0x51 => 121,
            0x52 => 114,
            0x53 => 117,
            0x5b if win_to_command => 55,
            0x5c if win_to_command => 54,
            _ => return None,
        });
    }
    Some(match scan {
        0x01 => 53,
        0x02 => 18,
        0x03 => 19,
        0x04 => 20,
        0x05 => 21,
        0x06 => 23,
        0x07 => 22,
        0x08 => 26,
        0x09 => 28,
        0x0a => 25,
        0x0b => 29,
        0x0c => 27,
        0x0d => 24,
        0x0e => 51,
        0x0f => 48,
        0x10 => 12,
        0x11 => 13,
        0x12 => 14,
        0x13 => 15,
        0x14 => 17,
        0x15 => 16,
        0x16 => 32,
        0x17 => 34,
        0x18 => 31,
        0x19 => 35,
        0x1a => 33,
        0x1b => 30,
        0x1c => 36,
        0x1d => 59,
        0x1e => 0,
        0x1f => 1,
        0x20 => 2,
        0x21 => 3,
        0x22 => 5,
        0x23 => 4,
        0x24 => 38,
        0x25 => 40,
        0x26 => 37,
        0x27 => 41,
        0x28 => 39,
        0x29 => 50,
        0x2a => 56,
        0x2b => 42,
        0x2c => 6,
        0x2d => 7,
        0x2e => 8,
        0x2f => 9,
        0x30 => 11,
        0x31 => 45,
        0x32 => 46,
        0x33 => 43,
        0x34 => 47,
        0x35 => 44,
        0x36 => 60,
        0x37 => 67,
        0x38 => 58,
        0x39 => 49,
        0x3a => 57,
        0x3b => 122,
        0x3c => 120,
        0x3d => 99,
        0x3e => 118,
        0x3f => 96,
        0x40 => 97,
        0x41 => 98,
        0x42 => 100,
        0x43 => 101,
        0x44 => 109,
        0x45 => 71,
        0x46 => 107,
        0x47 => 89,
        0x48 => 91,
        0x49 => 92,
        0x4a => 78,
        0x4b => 86,
        0x4c => 87,
        0x4d => 88,
        0x4e => 69,
        0x4f => 83,
        0x50 => 84,
        0x51 => 85,
        0x52 => 82,
        0x53 => 65,
        0x56 => 10, // ISO extra key
        0x57 => 103,
        0x58 => 111,
        // JIS physical keys, when supplied by the Windows keyboard driver.
        0x70 => 104,
        0x73 => 94,
        0x79 => 104,
        0x7b => 102,
        0x7d => 93,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_layout_modifiers_and_keypad() {
        assert_eq!(translate(0x51, 0x1e, false, true), Some(0)); // layout-independent A position
        assert_eq!(translate(0x11, 0x1d, false, true), Some(59));
        assert_eq!(translate(0x11, 0x1d, true, true), Some(62));
        assert_eq!(translate(0x0d, 0x1c, true, true), Some(76));
        assert_eq!(translate(0x26, 0x48, true, true), Some(126));
        assert_eq!(translate(0x68, 0x48, false, true), Some(91));
        assert_eq!(translate(0x5b, 0x5b, true, false), None);
        assert_eq!(translate(0x5c, 0x5c, true, true), Some(54));
        assert_eq!(translate(0, 0xffff, false, true), None);
    }
}
