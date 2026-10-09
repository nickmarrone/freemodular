//! Slew limiter: the output follows knob/CV 1 at a limited speed.
//! Knobs: 1 input, 2 rise time, 3 fall time, 4 shape (linear -> exponential).
//! Gate holds the output (track and hold); Trig drops it to zero.

use super::{
    shared::{delta_t, knob_level},
    GateState, Input,
};
use crate::settings::knob_position;

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum SlewPhase {
    /// Settled at zero
    #[default]
    Idle,
    Rising,
    Falling,
    /// Settled at the input (or held by the gate)
    Settled,
}

#[derive(Copy, Clone, Default)]
pub struct SlewState {
    pub phase: SlewPhase,
    /// Output level in 12.20 fixed point, so a full-scale move is ~2^32 like a stage
    level: u32,
}

const LEVEL_SHIFT: u32 = 20;
/// How far the input must move before a settled output counts as rising or falling
/// again, so ADC noise doesn't flicker the LEDs and aux output
const HYSTERESIS: u32 = 16 << LEVEL_SHIFT;
/// Exponential approaches slow down forever; finish them at no less than this speed
const MIN_STEP: u32 = 1 << 18;

pub fn slew(state: &mut SlewState, input: &Input, cv: &[u16; 4]) -> (u16, bool) {
    let old_phase = state.phase;
    if input.trigger {
        state.level = 0;
    }
    let target = (knob_level(cv[0]) as u32) << LEVEL_SHIFT;
    let rising = target > state.level;
    let distance = target.abs_diff(state.level);

    let holding = matches!(input.gate, GateState::High | GateState::Rising);
    if holding {
        state.phase = SlewPhase::Settled;
    } else {
        // a full-scale move at this speed takes one stage time
        let linear = delta_t(if rising { cv[1] } else { cv[2] });
        if linear >= 1 << 30 {
            // a stage of under 4 samples: just jump
            state.level = target;
        } else {
            // a one-pole filter that gets within 2% of the input in one stage time:
            // distance * linear * 4 / 2^32, kept within 32 bits
            let exponential = (distance >> 22) * (linear >> 8);
            let shape = (knob_position(cv[3]) as u32 * 67) >> 8; // 0-255
            let blend = ((linear >> 8) * (256 - shape)).saturating_add((exponential >> 8) * shape);
            let step = u32::max(blend, u32::min(linear, MIN_STEP));
            if distance <= step {
                state.level = target;
            } else if rising {
                state.level += step;
            } else {
                state.level -= step;
            }
        }

        // a moving output keeps moving until it arrives; a settled one only starts
        // moving again for a real change of the input
        let remaining = target.abs_diff(state.level);
        let moving = matches!(state.phase, SlewPhase::Rising | SlewPhase::Falling);
        state.phase = if remaining > HYSTERESIS || (moving && remaining > 0) {
            if target > state.level {
                SlewPhase::Rising
            } else {
                SlewPhase::Falling
            }
        } else if state.level < HYSTERESIS {
            SlewPhase::Idle
        } else {
            SlewPhase::Settled
        };
    }

    ((state.level >> LEVEL_SHIFT) as u16, state.phase != old_phase)
}
