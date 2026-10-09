use super::{
    shared::{lerp, read_cv, step_time, CvType},
    GateState, Input, MAX_DAC_VALUE,
};

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
) -> (u16, bool) {
    if input.trigger && input.gate != GateState::Rising {
        // Ping: soft-return to the attack stage from the current level. Without a
        // gate held, the envelope then runs attack -> decay -> release on its own.
        // (gate falling at the same moment counts as no gate)
        *artificial_gate = input.gate != GateState::High;
        *time = get_adsr_inverse_attack(last_value);
        *phase = AdsrState::Attack;
        let (value, _) = compute_adsr_value(phase, time, cv);
        return (value, true);
    }

    match input.gate {
        GateState::High => compute_adsr_value(phase, time, cv),
        GateState::Low => {
            let (value, rollover) = compute_adsr_value(phase, time, cv);
            if *artificial_gate && *phase == AdsrState::Sustain {
                // a ping has no gate to hold the sustain stage
                *artificial_gate = false;
                *phase = AdsrState::Release;
                *time = get_adsr_inverse_release(value);
            }
            (value, rollover)
        }
        GateState::Rising => {
            *artificial_gate = false;
            *time = get_adsr_inverse_attack(last_value);
            *phase = AdsrState::Attack;
            let (value, _) = compute_adsr_value(phase, time, cv);
            (value, true)
        }
        GateState::Falling => {
            *phase = AdsrState::Release;
            *time = get_adsr_inverse_release(last_value);
            let (value, _) = compute_adsr_value(phase, time, cv);
            (value, true)
        }
    }
}

fn get_adsr_inverse_attack(current_value: u16) -> u32 {
    (current_value as u32) << 20
}

fn get_adsr_inverse_release(current_value: u16) -> u32 {
    ((MAX_DAC_VALUE - current_value) as u32) << 20
}

fn compute_adsr_value(phase: &mut AdsrState, time: &mut u32, cv: &[u16; 4]) -> (u16, bool) {
    let scale = |input: u32| (input >> 20) as u16;
    let get_sustain = || {
        let cv_frac = read_cv::<{ CvType::Linear }>(cv[2]);
        let scaled = ((cv_frac.numerator as u32 * (MAX_DAC_VALUE + 1) as u32)
            / cv_frac.denominator as u32) as u16;
        u16::min(scaled, MAX_DAC_VALUE)
    };

    match phase {
        AdsrState::Wait => (0, false),
        AdsrState::Attack => {
            let (t, rollover) = step_time(time, cv[0]);
            if rollover {
                // with sustain all the way up, decay would just hold the peak
                *phase = if get_sustain() >= MAX_DAC_VALUE {
                    AdsrState::Sustain
                } else {
                    AdsrState::Decay
                };
            }
            (scale(t), rollover)
        }
        AdsrState::Decay => {
            let (t, rollover) = step_time(time, cv[1]);
            if rollover {
                *phase = AdsrState::Sustain;
            }
            let sustain = get_sustain();
            let scaled = lerp((t >> 16) as u16, sustain, MAX_DAC_VALUE);
            (sustain + (MAX_DAC_VALUE - scaled), rollover)
        }
        AdsrState::Sustain => (get_sustain(), false),
        AdsrState::Release => {
            let (t, rollover) = step_time(time, cv[3]);
            if rollover {
                *phase = AdsrState::Wait;
            }
            (MAX_DAC_VALUE.saturating_sub(scale(t)), rollover)
        }
    }
}
