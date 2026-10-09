//! Bouncing ball: a Trig drops a ball, and the output is its height as it bounces,
//! each bounce lower and quicker than the last, until it comes to rest.
//! Knobs: 1 drop time, 2 bounciness, 3 gravity (left: straight lines, middle:
//! parabolas, right: sharper), 4 drop height.
//! Holding the gate lifts the ball and holds it; releasing the gate drops it.
//! Each impact is an end of fall, so the aux end-of-fall pulse fires on every bounce.

use super::{
    shared::{advance, delta_t, knob_level, scale_dt},
    GateState, Input,
};
use crate::settings::knob_position;

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum BallPhase {
    #[default]
    Rest,
    /// Being lifted by the gate
    Lift,
    /// Held up by the gate
    Held,
    Down,
    Up,
}

#[derive(Copy, Clone, Default)]
pub struct BallState {
    pub phase: BallPhase,
    pub bounces: u8,
    /// Height of the current bounce, 0-4095
    apex: u16,
    /// How much faster this bounce is than the first drop (16.16 fixed point)
    speed: u32,
}

/// Bounces below this height stop the ball
const MIN_APEX: u16 = 8;
/// Bounces this much faster than the first drop stop the ball (16.16 fixed point)
const MAX_SPEED: u32 = 1 << 23;

/// Fraction of the height lost after time `u` (0-65535) of a fall from the apex:
/// u, u^2 (real gravity) or u^4, blended by the gravity knob
fn gravity(u: u16, cv: u16) -> u16 {
    const MIDDLE: u32 = 489;
    // multiplying by 134 and shifting by 16 divides by ~489 (split into two shifts
    // to stay within u32)
    let u = u as u32;
    let u2 = (u * u) >> 16;
    let position = knob_position(cv) as u32;
    (if position < MIDDLE {
        u - (((((u - u2) * position) >> 8) * 134) >> 8)
    } else {
        let u4 = (u2 * u2) >> 16;
        u2 - (((((u2 - u4) * (position - MIDDLE)) >> 8) * 134) >> 8)
    }) as u16
}

fn drop_ball(state: &mut BallState, time: &mut u32, from: u16) {
    state.phase = BallPhase::Down;
    state.apex = from;
    state.speed = 1 << 16;
    state.bounces = 0;
    *time = 0;
}

pub fn ball(
    state: &mut BallState,
    time: &mut u32,
    last_value: u16,
    input: &Input,
    cv: &[u16; 4],
) -> (u16, bool) {
    let mut changed = false;
    let top = knob_level(cv[3]);
    match input.gate {
        GateState::Rising => {
            // lift from wherever the ball is; `time` holds the height in 12.20
            state.phase = BallPhase::Lift;
            *time = (last_value as u32) << 20;
            changed = true;
        }
        GateState::Falling => {
            if matches!(state.phase, BallPhase::Lift | BallPhase::Held) {
                drop_ball(state, time, last_value);
                changed = true;
            }
        }
        GateState::Low if input.trigger => {
            drop_ball(state, time, top);
            changed = true;
        }
        _ => {}
    }

    let dt = delta_t(cv[0]);
    match state.phase {
        BallPhase::Rest => (0, changed),
        BallPhase::Lift => {
            // straight up, at the speed of a full-height drop
            *time = time.saturating_add(dt);
            if *time >= (top as u32) << 20 {
                state.phase = BallPhase::Held;
                return (top, true);
            }
            ((*time >> 20) as u16, changed)
        }
        BallPhase::Held => (top, changed),
        BallPhase::Down => {
            let (t, rollover) = advance(time, scale_dt(dt, state.speed));
            let fallen = gravity((t >> 16) as u16, cv[2]);
            let value = ((state.apex as u32 * (0xFFFF - fallen as u32)) >> 16) as u16;
            if rollover {
                // impact: the next bounce is e^2 as high and takes e as long
                let e = knob_position(cv[1]) as u32 * 63; // up to ~0.94
                let apex = (((state.apex as u32 * e) >> 16) * e) >> 16;
                let e8 = e >> 8;
                if apex < MIN_APEX as u32 || e8 == 0 || state.speed >= MAX_SPEED {
                    state.phase = BallPhase::Rest;
                } else {
                    state.phase = BallPhase::Up;
                    state.apex = apex as u16;
                    state.speed = (state.speed << 8) / e8;
                    state.bounces = state.bounces.wrapping_add(1);
                }
            }
            (value, changed || rollover)
        }
        BallPhase::Up => {
            let (t, rollover) = advance(time, scale_dt(dt, state.speed));
            let below = gravity(0xFFFF - (t >> 16) as u16, cv[2]);
            let value = ((state.apex as u32 * (0xFFFF - below as u32)) >> 16) as u16;
            if rollover {
                state.phase = BallPhase::Down;
            }
            (value, changed || rollover)
        }
    }
}
