use core::marker::ConstParamTy;
use core::sync::atomic::{AtomicBool, Ordering};
use fixed::{types::extra::U16, FixedU16};

use crate::exponential_curves::{exp_curve, exp_curve_inverse};
use crate::settings::{knob_position, Curve};

use super::MAX_DAC_VALUE;

/// When set, every stage takes 10x longer (up to 100 s instead of 10 s)
static LONG_TIME_RANGE: AtomicBool = AtomicBool::new(false);

pub fn set_long_time_range(long: bool) {
    LONG_TIME_RANGE.store(long, Ordering::Relaxed);
}

#[derive(Copy, Clone)]
pub struct Fraction<T> {
    pub numerator: T,
    pub denominator: T,
}

#[derive(PartialEq, Eq, ConstParamTy)]
pub enum CvType {
    Linear,
    Exponential,
}

/**
Transforms a raw cv value into a usable fraction of the maximum.
- Inverts value to compensate for the inverting amplifier in hardware
- Shifts values slightly to account for the fact that the input voltage
    is limited to a slightly smaller range than the DAC can read
- Applies a simple piecewise exponential curve to make the knobs more usable
*/
pub fn read_cv<const CURVE: CvType>(cv: u16) -> Fraction<u16> {
    // ADC reads up to 1023, but voltage doesn't go all the way to 5v
    const MAX_ADC_VALUE: u16 = 977;
    // CV is inverted in hardware; correct for that here
    let x = MAX_ADC_VALUE.saturating_sub(cv);

    let numerator = match CURVE {
        CvType::Linear => x,
        CvType::Exponential => {
            if x < 512 {
                x / 4
            } else if x < 768 {
                x - 384
            } else {
                3 * x - 1920
            }
        }
    };

    let denominator = match CURVE {
        CvType::Linear => MAX_ADC_VALUE,
        // the piecewise function isn't perfect, the range is a little larger
        // than the domain. It actually goes to 1011. Round to 1024 for performance
        CvType::Exponential => 1024,
    };

    Fraction {
        numerator: u16::min(numerator, denominator),
        denominator,
    }
}

/**
Transforms a raw cv value into a fixed point number between 0 and 1.
- Inverts value to compensate for the inverting amplifier in hardware
- Shifts values slightly to account for the fact that the input voltage
    is limited to a slightly smaller range than the DAC can read
*/
pub fn read_cv_signed_fixed(cv: u16) -> (FixedU16<U16>, bool) {
    // ADC reads up to 1023, but voltage doesn't go all the way to 5v
    const MAX_ADC_VALUE: u16 = 977;
    const MIDPOINT: u16 = MAX_ADC_VALUE / 2;
    let x = MAX_ADC_VALUE.saturating_sub(cv);

    if x > MIDPOINT {
        (FixedU16::<U16>::from_bits(x - MIDPOINT << 7), false)
    } else {
        (FixedU16::<U16>::from_bits(MIDPOINT - x << 7), true)
    }
}

/// Phase increment per sample for a stage whose length is set by `cv`
pub fn delta_t(cv: u16) -> u32 {
    // 10 seconds
    const MAX_PHASE_TIME_MICROS: u32 = 10 * 1000 * 1000;
    // must match the DAC update timer in main.rs (2083.3 Hz)
    const MICROS_PER_STEP: u32 = 480;
    const MAX_STEPS_PER_CYCLE: u32 = MAX_PHASE_TIME_MICROS / MICROS_PER_STEP;
    let cv_fraction = read_cv::<{ CvType::Exponential }>(cv);
    let max_steps = if LONG_TIME_RANGE.load(Ordering::Relaxed) {
        MAX_STEPS_PER_CYCLE * 10
    } else {
        MAX_STEPS_PER_CYCLE
    };
    // the exponential response's denominator is 1024, so shift instead of dividing
    debug_assert!(cv_fraction.denominator == 1024);
    let mut actual_steps_per_cycle = (cv_fraction.numerator as u32 * max_steps) >> 10;
    if actual_steps_per_cycle == 0 {
        actual_steps_per_cycle = 1;
    }

    u32::MAX / actual_steps_per_cycle
}

pub fn step_time(t: &mut u32, cv: u16) -> (u32, bool) {
    advance(t, delta_t(cv))
}

/// Advances the phase `t` by `dt`. At the end of the stage, returns (u32::MAX, true)
/// and resets `t` to 0.
pub fn advance(t: &mut u32, dt: u32) -> (u32, bool) {
    *t = t.saturating_add(dt);
    let rollover = *t == u32::MAX;
    let before_rollover = *t;
    if rollover {
        *t = 0;
    }
    (before_rollover, rollover)
}

/// `dt` sped up by `speed` (16.16 fixed point, 1.0 = unchanged, up to 2^23),
/// saturating. Split into 32-bit multiplies since 64-bit math is very slow on AVR.
pub fn scale_dt(dt: u32, speed: u32) -> u32 {
    debug_assert!(speed < 1 << 23);
    let high = (dt >> 16).saturating_mul(speed);
    let low = ((dt & 0xFFFF) * (speed >> 8)) >> 8;
    high.saturating_add(low)
}

/// `x * knob_position(cv) / MAX_ADC_VALUE` without a division, for x < 2^15
pub fn scale_by_knob(x: u32, cv: u16) -> u32 {
    // 67 / 65536 ~= 1 / 977
    debug_assert!(x < 1 << 15);
    (x * knob_position(cv) as u32 * 67) >> 16
}

/// Knob position as an output level from 0 to 4095
pub fn knob_level(cv: u16) -> u16 {
    // 4292 / 1024 ~= 4095 / 977, without a division
    u32::min((knob_position(cv) as u32 * 4292) >> 10, MAX_DAC_VALUE as u32) as u16
}

/// Scales `value` (0-4095) by `amplitude` (0-4096 = 0-1)
pub fn scale_level(value: u16, amplitude: u16) -> u16 {
    ((value as u32 * amplitude as u32) >> 12) as u16
}

/// xorshift32
pub fn next_random(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

pub fn step_time_no_rollover(t: &mut u32, cv: u16) -> u32 {
    *t = t.saturating_add(delta_t(cv));
    *t
}

pub fn lerp(x: u16, min: u16, max: u16) -> u16 {
    debug_assert!(min <= max);
    let range = max - min;
    ((x as u32 * range as u32) >> 16) as u16 + min
}

/// Level (0-4095) at phase `t` of a rising stage with the given curve
pub fn shape(t: u32, curve: Curve) -> u16 {
    let (c, c_negative) = curve;
    if c == 0 {
        // linear; skip the curve math
        (t >> 20) as u16
    } else {
        exp_curve(FixedU16::<U16>::from_bits((t >> 16) as u16), c, c_negative)
    }
}

/// Phase at which a rising stage with the given curve is at `level` (0-4095), so a
/// stage can restart from the current output without a jump
pub fn shape_inverse(level: u16, curve: Curve) -> u32 {
    let (c, c_negative) = curve;
    if c == 0 {
        (level as u32) << 20
    } else {
        let level_frac = FixedU16::<U16>::from_bits(level << 4);
        (exp_curve_inverse(level_frac, c, c_negative).to_bits() as u32) << 16
    }
}
