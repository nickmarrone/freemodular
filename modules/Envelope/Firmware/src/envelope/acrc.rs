use super::{
    shared::{read_cv_signed_fixed, shape, shape_inverse, step_time},
    GateState, Input, MAX_DAC_VALUE,
};
use crate::settings::{EnvelopeConfig, GateBehaviour};

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum AcrcState {
    #[default]
    Wait,
    Attack,
    Hold,
    Release,
}

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum AcrcLoopState {
    #[default]
    Attack,
    Release,
}

pub fn acrc(
    phase: &mut AcrcState,
    time: &mut u32,
    last_value: u16,
    input: &Input,
    cv: &[u16; 4],
    artificial_gate: &mut bool,
    config: &EnvelopeConfig,
) -> (u16, bool) {
    // in legato, nothing restarts the envelope until it is releasing
    let ignore_retrigger = config.gate_behaviour == GateBehaviour::Legato
        && matches!(*phase, AcrcState::Attack | AcrcState::Hold);

    match input.gate {
        GateState::High => compute_acrc_value(phase, time, cv, config, true),
        GateState::Rising => {
            // the gate takes over holding the envelope open from a ping
            *artificial_gate = false;
            if ignore_retrigger {
                return compute_acrc_value(phase, time, cv, config, true);
            }
            start_attack(phase, time, last_value, cv, config);
            let (value, _) = compute_acrc_value(phase, time, cv, config, true);
            (value, true)
        }
        GateState::Falling => {
            *phase = AcrcState::Release;
            *time = shape_inverse(MAX_DAC_VALUE - last_value, read_cv_signed_fixed(cv[3]));
            let (value, _) = compute_acrc_value(phase, time, cv, config, false);
            (value, true)
        }
        GateState::Low => {
            if input.trigger && !ignore_retrigger {
                start_attack(phase, time, last_value, cv, config);
                *artificial_gate = true;
                let (value, _) = compute_acrc_value(phase, time, cv, config, false);
                return (value, true);
            }

            let (value, rollover) = compute_acrc_value(phase, time, cv, config, false);

            if *artificial_gate {
                if rollover && *phase == AcrcState::Hold {
                    *phase = AcrcState::Release;
                }
            }

            (value, rollover)
        }
    }
}

fn start_attack(
    phase: &mut AcrcState,
    time: &mut u32,
    last_value: u16,
    cv: &[u16; 4],
    config: &EnvelopeConfig,
) {
    *phase = AcrcState::Attack;
    *time = if config.gate_behaviour == GateBehaviour::Reset {
        0
    } else {
        // continue from the current level
        shape_inverse(last_value, read_cv_signed_fixed(cv[1]))
    };
}

fn compute_acrc_value(
    phase: &mut AcrcState,
    time: &mut u32,
    cv: &[u16; 4],
    config: &EnvelopeConfig,
    gate_high: bool,
) -> (u16, bool) {
    let cycle = config.gate_behaviour == GateBehaviour::Cycle && gate_high;
    match phase {
        AcrcState::Wait => (0, false),
        AcrcState::Attack => {
            let (t, rollover) = acrc_segment(time, cv[0], cv[1], false);
            if rollover {
                // cycling skips the hold and goes straight back down
                *phase = if cycle {
                    AcrcState::Release
                } else {
                    AcrcState::Hold
                };
            }
            (t, rollover)
        }
        AcrcState::Hold => (MAX_DAC_VALUE, false),
        AcrcState::Release => {
            let (t, rollover) = acrc_segment(time, cv[2], cv[3], true);
            if rollover {
                *phase = if cycle {
                    AcrcState::Attack
                } else {
                    AcrcState::Wait
                };
            }
            (t, rollover)
        }
    }
}

pub fn acrc_loop(
    phase: &mut AcrcLoopState,
    time: &mut u32,
    input: &Input,
    cv: &[u16; 4],
) -> (u16, bool) {
    if input.trigger {
        *time = 0;
        let did_change = *phase == AcrcLoopState::Release;
        *phase = AcrcLoopState::Attack;
        return (0, did_change);
    }

    match phase {
        AcrcLoopState::Attack => {
            let (value, rollover) = acrc_segment(time, cv[0], cv[1], false);
            if rollover {
                *phase = AcrcLoopState::Release;
            }
            (value, rollover)
        }
        AcrcLoopState::Release => {
            let (value, rollover) = acrc_segment(time, cv[2], cv[3], true);
            if rollover && input.gate == GateState::High {
                *time = u32::MAX;
                return (0, false);
            }
            if rollover {
                *phase = AcrcLoopState::Attack;
            }
            (value, rollover)
        }
    }
}

#[inline(never)]
fn acrc_segment(time: &mut u32, raw_cv_len: u16, raw_cv_c: u16, invert: bool) -> (u16, bool) {
    let (t, rollover) = step_time(time, raw_cv_len);
    let value = shape(t, read_cv_signed_fixed(raw_cv_c));
    (if invert { MAX_DAC_VALUE - value } else { value }, rollover)
}
