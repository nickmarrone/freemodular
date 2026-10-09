//! Burst / ratchet: a Trig (or the gate going high) fires a burst of short AR pulses.
//! Knobs: 1 pulse time, 2 number of pulses (1-16), 3 pulse heights (left: each pulse
//! lower than the last, middle: all the same, right: each higher), 4 curve.
//! Holding the gate repeats the burst.

use super::{
    shared::{advance, delta_t, read_cv_signed_fixed, scale_by_knob, scale_level, shape},
    GateState, Input, MAX_DAC_VALUE,
};

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum BurstPhase {
    #[default]
    Idle,
    Rise,
    Fall,
}

#[derive(Copy, Clone, Default)]
pub struct BurstState {
    pub phase: BurstPhase,
    /// Index of the current pulse in the burst
    pub pulse: u8,
    /// Height of the current pulse, 0-4096
    amplitude: u16,
}

const MAX_PULSES: u16 = 16;

fn pulse_count(cv: u16) -> u8 {
    1 + u32::min(scale_by_knob(MAX_PULSES as u32, cv), MAX_PULSES as u32 - 1) as u8
}

/// Height of pulse `pulse` of `count`. Each step changes the height by up to half.
fn pulse_amplitude(pulse: u8, count: u8, cv: u16) -> u16 {
    let (change, shrink) = read_cv_signed_fixed(cv);
    let factor = 0x10000 - (change.to_bits() as u32 >> 1);
    let steps = if shrink {
        pulse
    } else {
        count.saturating_sub(pulse + 1)
    };
    let mut amplitude: u32 = 4096;
    for _ in 0..steps {
        amplitude = (amplitude * factor) >> 16;
    }
    amplitude as u16
}

fn start_pulse(state: &mut BurstState, time: &mut u32, pulse: u8, cv: &[u16; 4]) {
    state.phase = BurstPhase::Rise;
    state.pulse = pulse;
    state.amplitude = pulse_amplitude(pulse, pulse_count(cv[1]), cv[2]);
    *time = 0;
}

pub fn burst(state: &mut BurstState, time: &mut u32, input: &Input, cv: &[u16; 4]) -> (u16, bool) {
    let mut changed = false;
    if input.trigger || input.gate == GateState::Rising {
        start_pulse(state, time, 0, cv);
        changed = true;
    }

    // each half of a pulse takes half the pulse time
    let dt = delta_t(cv[0]).saturating_mul(2);
    let curve = read_cv_signed_fixed(cv[3]);
    match state.phase {
        BurstPhase::Idle => (0, changed),
        BurstPhase::Rise => {
            let (t, rollover) = advance(time, dt);
            if rollover {
                state.phase = BurstPhase::Fall;
            }
            (scale_level(shape(t, curve), state.amplitude), changed || rollover)
        }
        BurstPhase::Fall => {
            let (t, rollover) = advance(time, dt);
            let value = scale_level(MAX_DAC_VALUE - shape(t, curve), state.amplitude);
            if rollover {
                let next = state.pulse + 1;
                if next < pulse_count(cv[1]) {
                    start_pulse(state, time, next, cv);
                } else if input.gate == GateState::High {
                    start_pulse(state, time, 0, cv);
                } else {
                    state.phase = BurstPhase::Idle;
                }
            }
            (value, changed || rollover)
        }
    }
}
