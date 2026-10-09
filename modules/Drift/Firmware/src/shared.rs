/// Time between output samples (2.5kHz), set by TIMER0 in main.rs
pub const MICROS_PER_SAMPLE: u32 = 400;

use avr_progmem::progmem;

progmem! {
    /// round(2^15 * 2^(j/16)) for j in 0..16
    static progmem EXP2_SIXTEENTHS: [u16; 16] = [
        32768, 34219, 35734, 37316, 38968, 40693, 42495, 44376, 46341, 48393, 50535, 52773,
        55109, 57549, 60097, 62757,
    ];
}

/// 2^15 * 2^(j/16), for j < 16
fn exp2_fraction(j: u16) -> u32 {
    EXP2_SIXTEENTHS.load_at((j & 15) as usize) as u32
}

/// 2^15 * 2^(sixteenths / 16), for sixteenths < 272
pub fn exp2_sixteenths(sixteenths: u16) -> u32 {
    exp2_fraction(sixteenths) << (sixteenths >> 4)
}

/// Phase increment per sample for 1/40 Hz: one cycle of the 32-bit phase counter
/// every 40 seconds
const PHASE_INCREMENT_BASE: u32 =
    ((1u64 << 32) * MICROS_PER_SAMPLE as u64 / (40 * 1_000_000)) as u32;

/// Phase increment for `step` sixteenths of an octave above 1/40 Hz
fn phase_increment_at(step: u16) -> u32 {
    let within_octave = PHASE_INCREMENT_BASE * exp2_fraction(step);
    (within_octave >> 15) << (step >> 4)
}

/**
Gets the phase increment for the frequency given by the knob and cv inputs.
- `knob` is the raw ADC reading of the knob position [0,1023] scaled as if it spanned 12v
- `cv` is the raw ADC reading of the CV input [0,1023], spanning [0,5] volts
- `offset` is a signed delta (-2^15,2^15), scaled to represent +/- 2.5 volts

All inputs are summed, clamped to the 0-12v range, and track 1v/oct from 1/40 Hz
at 0v up to ~100 Hz at 12v. The result is the amount to add to a 32-bit phase
counter each sample so that it rolls over at that frequency.
*/
pub fn get_delta_t(knob: u16, cv: u16, offset: i16) -> u32 {
    // knob scaled as if it spanned 12v
    let knob_12v = (knob * 12) / 5;
    const MAX_KNOB_VALUE: u16 = (1023 * 12) / 5;
    let mut sum = knob_12v + cv;
    sum = sum.saturating_add_signed(offset / 64);
    sum = u16::min(sum, MAX_KNOB_VALUE);

    // 1v is 1024/5 ADC steps, so `sum * 20` is in 256ths of a sixteenth of an octave
    let octaves = sum * 20;
    let step = octaves >> 8;
    let fraction = (octaves & 0xFF) as u32;
    let low = phase_increment_at(step);
    let high = phase_increment_at(step + 1);
    low + (((high - low) * fraction) >> 8)
}

pub trait DriftModule {
    /**
    Advance the module one time step and compute the output at that point.
    Returns a value between 0 and 4095.
    */
    fn step(&mut self, cv: &[u16; 4]) -> u16;
}
