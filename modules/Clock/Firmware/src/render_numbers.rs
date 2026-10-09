use avr_progmem::progmem;

use crate::clock::{DIV_STOP, TUPLET_DOTTED, TUPLET_TRIPLET};

/*
The fonts use a custom code page (ASCII '0'..'z') to save space, so some symbols are
remapped: ';' => '-', '`' => '+', '_' => '/', '^' => '%', '\' => '.'
*/
pub const CHAR_MINUS: u8 = b';';
pub const CHAR_PLUS: u8 = b'`';
pub const CHAR_DIVIDE: u8 = b'_';
pub const CHAR_PERCENT: u8 = b'^';
pub const CHAR_PERIOD: u8 = b'\\';

progmem! {
    static progmem WORDS: [[u8; 4]; 11] = [
        *b"Off\0", *b"Trip", *b"Dot\0", *b"Now\0", *b"Beat", *b"Bar\0",
        *b"TRIG", *b"INVT", *b"STOP", *b"RUN\0", *b"RST\0",
    ];
}

pub const WORD_OFF: u8 = 0;
pub const WORD_STOP_NOW: u8 = 3;
pub const WORD_TRIG: u8 = 6;
pub const WORD_INVT: u8 = 7;
pub const WORD_STOP: u8 = 8;

/// Copies a short word from PROGMEM into the buffer
#[inline(never)]
pub fn word(buffer: &mut [u8; 5], index: u8) -> &[u8] {
    let w = WORDS.load_at(index as usize);
    buffer[..4].copy_from_slice(&w);
    let len = w.iter().position(|c| *c == 0).unwrap_or(4);
    &buffer[..len]
}

/// Up to 3 digits, no leading zeros
#[inline(never)]
pub fn u16_to_str_b10(buffer: &mut [u8], mut n: u16) -> &mut [u8] {
    debug_assert!(buffer.len() >= 3);
    const POWERS: [u16; 3] = [100, 10, 1];
    let mut cursor = 2u8;
    for i in 0u8..3u8 {
        let power_value = POWERS[i as usize];
        let digit = (n / power_value) as u8;
        n %= power_value;
        buffer[i as usize] = b'0' + digit;
        if digit != 0 && cursor == 2u8 {
            cursor = i
        }
    }
    &mut buffer[(cursor as usize)..3]
}

#[inline(never)]
pub fn i8_to_str_b10(buffer: &mut [u8], n: i8) -> &mut [u8] {
    debug_assert!(buffer.len() >= 4);
    if n == 0 {
        buffer[0] = b'0';
        return &mut buffer[..1];
    }
    let sign = if n < 0 { CHAR_MINUS } else { CHAR_PLUS };
    let len = u16_to_str_b10(&mut buffer[1..], n.unsigned_abs() as u16).len();
    let start = 3 - len;
    buffer[start] = sign;
    &mut buffer[start..4]
}

/// Formats a channel division, e.g. "x4", "/16t" (triplet), "x2." (dotted), "STOP"
pub fn tempo_to_str(buffer: &mut [u8; 5], division: i8, tuplet: u8) -> &[u8] {
    debug_assert!(division != 0);

    if division < -64 {
        return word(buffer, WORD_STOP + (DIV_STOP - division) as u8);
    }
    let symbol = if division < 0 { CHAR_DIVIDE } else { b'x' };
    let start = 4 - i8_to_str_b10(&mut buffer[..], division).len();
    buffer[start] = symbol;
    let end = match tuplet {
        TUPLET_TRIPLET => {
            buffer[4] = b't';
            5
        }
        TUPLET_DOTTED => {
            buffer[4] = CHAR_PERIOD;
            5
        }
        _ => 4,
    };
    &buffer[start..end]
}
