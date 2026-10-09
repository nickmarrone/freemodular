use core::mem::MaybeUninit;

use arduino_hal::Eeprom;
use avr_device::{atmega328p::EEPROM, interrupt};

use crate::ringbuffer::find_ringbuffer_head;

/**
The EEPROM write sequence requires setting EEPE within 4 cycles of EEMPE. avr-hal
doesn't disable interrupts around that, so any interrupt landing in between makes
the write silently fail. Wait for any previous write to finish first (so interrupts
aren't held off for milliseconds), then do the write itself in a critical section.
*/
fn write_byte_atomic(eeprom: &mut Eeprom, address: u16, value: u8) {
    let regs = unsafe { &*EEPROM::ptr() };
    while regs.eecr.read().eepe().bit_is_set() {}
    interrupt::free(|_| eeprom.write_byte(address, value));
}

fn erase_byte_atomic(eeprom: &mut Eeprom, address: u16) {
    let regs = unsafe { &*EEPROM::ptr() };
    while regs.eecr.read().eepe().bit_is_set() {}
    interrupt::free(|_| eeprom.erase_byte(address));
}

fn write_atomic(eeprom: &mut Eeprom, address: u16, data: &[u8]) {
    for (i, byte) in data.iter().enumerate() {
        write_byte_atomic(eeprom, address + i as u16, *byte);
    }
}

/**
Every time the writer is initialized (i.e. on device startup) the full object is
copied to a new location in EEPROM. From then on, updates can be made quickly in
place, while a mirrored copy of any relevant state is kept in memory by the user.
EEPROM should not be read from again until the next startup.
*/
pub struct WearLevelledEepromWriter<const SIZE: usize> {
    pub address: u16,
    pub version: u16,
    eeprom: Eeprom,
}

impl<const SIZE: usize> WearLevelledEepromWriter<SIZE> {
    const DATA_SIZE: u16 = SIZE as u16;
    const TOTAL_SIZE: u16 = 2 + Self::DATA_SIZE;

    #[inline(never)]
    pub fn init_and_advance(eeprom: EEPROM, memory: &mut [u8; SIZE], clear: bool) -> Self {
        let mut eep = arduino_hal::Eeprom::new(eeprom);

        if clear {
            Self::clear_all(&mut eep);
        }

        let (address, version) = Self::binary_search_for_monotonic_ringbuffer_head(&eep);

        let mut writer = Self {
            address,
            version,
            eeprom: eep,
        };

        if version == 0xFFFF {
            writer.address = 0;
            writer.version = 0;
            writer.write_data(memory);
        } else {
            *memory = writer.advance_and_copy();
        }

        writer
    }

    fn write_data(&mut self, data: &[u8; SIZE]) {
        let marker_bytes = self.version.to_le_bytes();
        erase_byte_atomic(&mut self.eeprom, self.address + 1);
        write_atomic(&mut self.eeprom, self.address + 2, data);
        write_byte_atomic(&mut self.eeprom, self.address + 0, marker_bytes[0]);
        write_byte_atomic(&mut self.eeprom, self.address + 1, marker_bytes[1]);
    }

    fn advance_and_copy(&mut self) -> [u8; SIZE] {
        let mut data: [u8; SIZE] = unsafe { MaybeUninit::uninit().assume_init() };
        self.eeprom.read(self.address + 2, &mut data).unwrap();

        let (new_address, new_version) = if (self.version + 1) >> 8 == 0xFF {
            // The version is about to collide with the "empty" marker (MSB 0xFF).
            // Start the ring over from the beginning. Resetting the address isn't
            // ideal for wear, but it keeps the order invariant which loads much faster.
            self.clear();
            (0, 0)
        } else {
            let mut new_address = self.address + Self::TOTAL_SIZE;
            if new_address + Self::TOTAL_SIZE > self.eeprom.capacity() {
                new_address = 0;
            }
            (new_address, self.version + 1)
        };

        erase_byte_atomic(&mut self.eeprom, new_address + 1);
        write_atomic(&mut self.eeprom, new_address + 2, &data);
        let new_version_bytes = new_version.to_le_bytes();
        write_byte_atomic(&mut self.eeprom, new_address, new_version_bytes[0]);
        write_byte_atomic(&mut self.eeprom, new_address + 1, new_version_bytes[1]);

        self.address = new_address;
        self.version = new_version;

        data
    }

    fn clear_all(eeprom: &mut Eeprom) {
        for address in (0..=eeprom.capacity() - Self::TOTAL_SIZE).step_by(Self::TOTAL_SIZE as usize)
        {
            erase_byte_atomic(eeprom, address + 1);
        }
    }

    fn clear(&mut self) {
        for address in
            (0..=self.eeprom.capacity() - Self::TOTAL_SIZE).step_by(Self::TOTAL_SIZE as usize)
        {
            if address != self.address {
                erase_byte_atomic(&mut self.eeprom, address + 1);
            }
        }
        erase_byte_atomic(&mut self.eeprom, self.address);
        erase_byte_atomic(&mut self.eeprom, self.address + 1);
    }

    pub fn update_byte(&mut self, offset: u16, byte: u8) {
        debug_assert!(offset < Self::DATA_SIZE);
        write_byte_atomic(&mut self.eeprom, self.address + 2 + offset, byte);
    }

    fn load_version_number(eeprom: &Eeprom, index: u16) -> u16 {
        let address = index * Self::TOTAL_SIZE;
        let msb = eeprom.read_byte(address + 1);
        if msb == 0xFF {
            return 0xFFFF;
        }

        let lsb = eeprom.read_byte(address);
        u16::from_le_bytes([lsb, msb])
    }

    pub fn binary_search_for_monotonic_ringbuffer_head(eeprom: &Eeprom) -> (u16, u16) {
        let len = eeprom.capacity() / Self::TOTAL_SIZE;
        let (index, version) =
            find_ringbuffer_head(len, |i| Self::load_version_number(eeprom, i));
        (index * Self::TOTAL_SIZE, version)
    }
}
