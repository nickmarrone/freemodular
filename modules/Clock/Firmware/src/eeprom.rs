use core::mem;

use avr_device::{atmega328p::EEPROM, interrupt};

use crate::clock::ClockConfig;

/*
EEPROM layout:

```text
0 ............................. PRESET_BASE ........................ 1024
| wear-levelled live config blocks | preset slot 0 | ... | preset slot 7 |
```

On startup, the latest live config block is loaded and then copied to the next block,
spreading writes out over the wear-levelled area since each EEPROM cell only survives
a limited number of writes. Each block is `[MAGIC, version, config...]`. The header is
written *after* the data so a block that was only partially written is never chosen.

All writes skip bytes that already hold the right value, so saving the whole config
after an edit only actually writes the byte(s) that changed.

This talks to the EEPROM registers directly instead of using the avr-hal driver: it
is much smaller, and it disables interrupts for the timed EEMPE -> EEPE sequence,
which the clock interrupt would otherwise be able to break.
*/

const CONFIG_SIZE: u16 = mem::size_of::<ClockConfig>() as u16;
const CAPACITY: u16 = 1024;
pub const NUM_PRESETS: u8 = 8;
const PRESET_BASE: u16 = CAPACITY - NUM_PRESETS as u16 * CONFIG_SIZE;
const BLOCK_SIZE: u16 = CONFIG_SIZE + 2;
const NUM_BLOCKS: u16 = PRESET_BASE / BLOCK_SIZE;
/// Identifies blocks written by this firmware/format version. Bump when the config
/// layout changes so old data is ignored instead of misinterpreted.
const MAGIC: u8 = 0xC2;

fn regs() -> &'static avr_device::atmega328p::eeprom::RegisterBlock {
    unsafe { &*EEPROM::ptr() }
}

fn read_byte(addr: u16) -> u8 {
    let ee = regs();
    while ee.eecr.read().eepe().bit_is_set() {}
    ee.eear.write(|w| w.bits(addr));
    ee.eecr.write(|w| w.eere().set_bit());
    ee.eedr.read().bits()
}

fn write_byte(addr: u16, value: u8) {
    if read_byte(addr) == value {
        return;
    }
    let ee = regs();
    ee.eedr.write(|w| w.bits(value));
    interrupt::free(|_| {
        // EEPE must be set within 4 cycles of EEMPE
        ee.eecr.write(|w| w.eempe().set_bit());
        ee.eecr.write(|w| w.eempe().set_bit().eepe().set_bit());
    });
}

fn as_bytes(config: &ClockConfig) -> &[u8; CONFIG_SIZE as usize] {
    unsafe { mem::transmute(config) }
}

#[inline(never)]
fn write_config(addr: u16, config: &ClockConfig) {
    for (i, byte) in as_bytes(config).iter().enumerate() {
        write_byte(addr + i as u16, *byte);
    }
}

/// Reads a config into `config` if the stored data is valid. Returns success.
#[inline(never)]
fn read_config(addr: u16, config: &mut ClockConfig) -> bool {
    let mut candidate = ClockConfig::new();
    let raw: &mut [u8; CONFIG_SIZE as usize] = unsafe { mem::transmute(&mut candidate) };
    for (i, byte) in raw.iter_mut().enumerate() {
        *byte = read_byte(addr + i as u16);
    }
    if candidate.is_valid() {
        *config = candidate;
        true
    } else {
        false
    }
}

/// Imports settings saved by older firmware, which stored `[version, config]` blocks
/// of 35 bytes with 4 bytes per channel (division, swing, pulse width, phase) followed
/// by the BPM. Leaves `config` untouched if nothing valid is found.
#[inline(never)]
fn migrate_legacy_config(config: &mut ClockConfig) {
    const LEGACY_BLOCK_SIZE: u16 = 35;
    let mut latest: Option<(u16, u8)> = None;
    let mut addr = 0;
    while addr + LEGACY_BLOCK_SIZE <= CAPACITY {
        let version = read_byte(addr);
        if version != 0xff && latest.map_or(true, |(_, v)| version > v) {
            latest = Some((addr, version));
        }
        addr += LEGACY_BLOCK_SIZE;
    }
    if let Some((addr, _)) = latest {
        let mut legacy = ClockConfig::new();
        let mut a = addr + 1;
        for channel in legacy.channels.iter_mut() {
            channel.division = read_byte(a) as i8;
            channel.swing = read_byte(a + 1);
            channel.pulse_width = read_byte(a + 2);
            channel.phase_shift = read_byte(a + 3) as i8;
            a += 4;
        }
        legacy.bpm10 = read_byte(a) as u16 * 10;
        if legacy.is_valid() {
            *config = legacy;
        }
    }
}

pub struct PersistanceManager {
    /// address of the config data in the active block
    offset: u16,
    dirty: bool,
    changed_at_ms: u32,
}

impl PersistanceManager {
    /// Loads the saved config (or leaves the default if there is none) and claims
    /// the next wear-levelling block
    #[inline(never)]
    pub fn new(clock_config: &mut ClockConfig) -> Self {
        let mut latest: Option<(u16, u8)> = None;
        for block in 0..NUM_BLOCKS {
            let addr = block * BLOCK_SIZE;
            if read_byte(addr) == MAGIC {
                let version = read_byte(addr + 1);
                if latest.map_or(true, |(_, v)| version > v) {
                    latest = Some((block, version));
                }
            }
        }

        let (block, version) = match latest {
            Some((block, version)) => {
                // If the saved config is invalid (corrupted EEPROM, a bug...) the
                // default config is kept
                if !read_config(block * BLOCK_SIZE + 2, clock_config) {
                    migrate_legacy_config(clock_config);
                }
                ((block + 1) % NUM_BLOCKS, version.wrapping_add(1))
            }
            None => {
                migrate_legacy_config(clock_config);
                (0, 0)
            }
        };
        if version == 0 && latest.is_some() {
            // version counter wrapped: invalidate every block so the new one is latest
            for b in 0..NUM_BLOCKS {
                write_byte(b * BLOCK_SIZE, 0xff);
            }
        }

        let addr = block * BLOCK_SIZE;
        write_config(addr + 2, clock_config);
        write_byte(addr + 1, version);
        write_byte(addr, MAGIC);

        Self {
            offset: addr + 2,
            dirty: false,
            changed_at_ms: 0,
        }
    }

    /// Note that the config changed. It will be written after a short delay so a
    /// burst of edits only costs one write.
    pub fn mark_dirty(&mut self, now_ms: u32) {
        self.dirty = true;
        self.changed_at_ms = now_ms;
    }

    /// Write pending changes if the config has been stable for a moment
    pub fn poll(&mut self, config: &ClockConfig, now_ms: u32) {
        if self.dirty && now_ms.wrapping_sub(self.changed_at_ms) > 1500 {
            self.save(config);
        }
    }

    #[inline(never)]
    pub fn save(&mut self, config: &ClockConfig) {
        write_config(self.offset, config);
        self.dirty = false;
    }

    pub fn save_preset(&mut self, slot: u8, config: &ClockConfig) {
        write_config(PRESET_BASE + slot as u16 * CONFIG_SIZE, config);
    }

    /// Returns false (leaving `config` untouched) if the slot is empty or invalid
    pub fn load_preset(&mut self, slot: u8, config: &mut ClockConfig) -> bool {
        read_config(PRESET_BASE + slot as u16 * CONFIG_SIZE, config)
    }
}
