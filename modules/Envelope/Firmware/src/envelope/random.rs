//! Random loop: a looping rise and fall whose timing and height are rerolled every
//! cycle. Knobs: 1 time, 2 time randomness (up to 2 octaves either way), 3 height
//! randomness, 4 curve. Trig restarts the cycle; holding the gate pauses it at zero.

use fixed::{types::extra::U16, FixedU16};

use super::{
    shared::{
        advance, delta_t, next_random, read_cv_signed_fixed, scale_by_knob, scale_dt, scale_level,
        shape,
    },
    GateState, Input, MAX_DAC_VALUE,
};
use crate::exponential_curves::exp2_16x;

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum RandomPhase {
    #[default]
    Rise,
    Fall,
}

#[derive(Copy, Clone)]
pub struct RandomState {
    pub phase: RandomPhase,
    /// Height of this cycle, 0-4096
    peak: u16,
    /// How much faster this stage runs than the knob says (16.16 fixed point)
    speed: u32,
    rng: u32,
}

impl Default for RandomState {
    fn default() -> Self {
        Self {
            phase: RandomPhase::Rise,
            peak: 4096,
            speed: 1 << 16,
            rng: 0x2545_F491,
        }
    }
}

/// Picks a new speed for the next stage: 2^e with e random in +/-2 octaves, scaled
/// by the time randomness knob
fn reroll_speed(state: &mut RandomState, cv: u16) {
    let random = next_random(&mut state.rng) as u16;
    let slower = random & 0x8000 != 0;
    let magnitude = (random & 0x7FFF) as u32;
    // e / 16 as a 16-bit fraction: up to 1/8 (2 octaves) at full randomness
    let x = scale_by_knob(magnitude, cv) >> 2;
    let factor = exp2_16x(FixedU16::<U16>::from_bits(x as u16)).to_bits();
    state.speed = if slower { u32::MAX / factor } else { factor };
}

fn reroll_peak(state: &mut RandomState, cv: u16) {
    let random = next_random(&mut state.rng) >> 20; // 0-4095
    state.peak = 4096 - scale_by_knob(random, cv) as u16;
}

pub fn random_loop(
    state: &mut RandomState,
    time: &mut u32,
    input: &Input,
    cv: &[u16; 4],
) -> (u16, bool) {
    let mut changed = false;
    if input.trigger {
        *time = 0;
        changed = state.phase != RandomPhase::Rise;
        state.phase = RandomPhase::Rise;
        reroll_peak(state, cv[2]);
        reroll_speed(state, cv[1]);
    }

    let dt = scale_dt(delta_t(cv[0]), state.speed);
    let curve = read_cv_signed_fixed(cv[3]);
    match state.phase {
        RandomPhase::Rise => {
            let (t, rollover) = advance(time, dt);
            let value = scale_level(shape(t, curve), state.peak);
            if rollover {
                state.phase = RandomPhase::Fall;
                reroll_speed(state, cv[1]);
            }
            (value, changed || rollover)
        }
        RandomPhase::Fall => {
            let (t, rollover) = advance(time, dt);
            if rollover && input.gate == GateState::High {
                // wait at zero until the gate is released
                *time = u32::MAX;
                return (0, changed);
            }
            let value = scale_level(MAX_DAC_VALUE - shape(t, curve), state.peak);
            if rollover {
                state.phase = RandomPhase::Rise;
                reroll_peak(state, cv[2]);
                reroll_speed(state, cv[1]);
            }
            (value, changed || rollover)
        }
    }
}
