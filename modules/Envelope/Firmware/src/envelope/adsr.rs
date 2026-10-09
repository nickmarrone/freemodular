use super::{
    shared::{lerp, read_cv, shape, shape_inverse, step_time, CvType},
    GateState, Input, MAX_DAC_VALUE,
};
use crate::settings::{EnvelopeConfig, GateBehaviour};

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum AdsrState {
    #[default]
    Wait,
    Attack,
    Decay,
    Sustain,
    Release,
}

pub fn adsr(
    phase: &mut AdsrState,
    time: &mut u32,
    last_value: u16,
    input: &Input,
    cv: &[u16; 4],
    artificial_gate: &mut bool,
    config: &EnvelopeConfig,
) -> (u16, bool) {
    // in legato, nothing restarts the envelope until it is releasing
    let ignore_retrigger = config.gate_behaviour == GateBehaviour::Legato
        && matches!(
            *phase,
            AdsrState::Attack | AdsrState::Decay | AdsrState::Sustain
        );

    if input.trigger && input.gate != GateState::Rising && !ignore_retrigger {
        // Ping: return to the attack stage. Without a gate held, the envelope then
        // runs attack -> decay -> release on its own.
        // (gate falling at the same moment counts as no gate)
        *artificial_gate = input.gate != GateState::High;
        start_attack(phase, time, last_value, config);
        let (value, _) = compute_adsr_value(phase, time, cv, config, false);
        return (value, true);
    }

    match input.gate {
        GateState::High => compute_adsr_value(phase, time, cv, config, true),
        GateState::Low => {
            let (value, rollover) = compute_adsr_value(phase, time, cv, config, false);
            if *artificial_gate && *phase == AdsrState::Sustain {
                // a ping has no gate to hold the sustain stage
                *artificial_gate = false;
                *phase = AdsrState::Release;
                *time = shape_inverse(MAX_DAC_VALUE - value, config.release_curve);
            }
            (value, rollover)
        }
        GateState::Rising => {
            // the gate takes over holding the envelope open from a ping
            *artificial_gate = false;
            if ignore_retrigger {
                return compute_adsr_value(phase, time, cv, config, true);
            }
            start_attack(phase, time, last_value, config);
            let (value, _) = compute_adsr_value(phase, time, cv, config, true);
            (value, true)
        }
        GateState::Falling => {
            *phase = AdsrState::Release;
            *time = shape_inverse(MAX_DAC_VALUE - last_value, config.release_curve);
            let (value, _) = compute_adsr_value(phase, time, cv, config, false);
            (value, true)
        }
    }
}

fn start_attack(phase: &mut AdsrState, time: &mut u32, last_value: u16, config: &EnvelopeConfig) {
    *phase = AdsrState::Attack;
    *time = if config.gate_behaviour == GateBehaviour::Reset {
        0
    } else {
        // continue from the current level
        shape_inverse(last_value, config.attack_curve)
    };
}

fn compute_adsr_value(
    phase: &mut AdsrState,
    time: &mut u32,
    cv: &[u16; 4],
    config: &EnvelopeConfig,
    gate_high: bool,
) -> (u16, bool) {
    let get_sustain = || {
        let cv_frac = read_cv::<{ CvType::Linear }>(cv[2]);
        let scaled = ((cv_frac.numerator as u32 * (MAX_DAC_VALUE + 1) as u32)
            / cv_frac.denominator as u32) as u16;
        u16::min(scaled, MAX_DAC_VALUE)
    };
    let cycle = config.gate_behaviour == GateBehaviour::Cycle && gate_high;

    match phase {
        AdsrState::Wait => (0, false),
        AdsrState::Attack => {
            let (t, rollover) = step_time(time, cv[0]);
            if rollover {
                // with sustain all the way up, decay would just hold the peak
                *phase = if !cycle && get_sustain() >= MAX_DAC_VALUE {
                    AdsrState::Sustain
                } else {
                    AdsrState::Decay
                };
            }
            (shape(t, config.attack_curve), rollover)
        }
        AdsrState::Decay => {
            let (t, rollover) = step_time(time, cv[1]);
            let sustain = get_sustain();
            if rollover {
                if cycle {
                    // loop attack -> decay for as long as the gate is held
                    *phase = AdsrState::Attack;
                    *time = shape_inverse(sustain, config.attack_curve);
                } else {
                    *phase = AdsrState::Sustain;
                }
            }
            let remaining = MAX_DAC_VALUE - shape(t, config.release_curve);
            (lerp(remaining << 4, sustain, MAX_DAC_VALUE), rollover)
        }
        AdsrState::Sustain => (get_sustain(), false),
        AdsrState::Release => {
            let (t, rollover) = step_time(time, cv[3]);
            if rollover {
                *phase = AdsrState::Wait;
            }
            (MAX_DAC_VALUE - shape(t, config.release_curve), rollover)
        }
    }
}
