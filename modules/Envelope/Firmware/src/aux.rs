use crate::envelope::{
    AcrcLoopState, AcrcState, AdsrState, AhrdState, BallPhase, BurstPhase, EnvelopeMode,
    RandomPhase, SlewPhase,
};
use crate::settings::AuxMode;

/// What the aux output can show about the current stage, for each aux mode
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AuxFlags {
    pub end_of_rise: bool,
    pub end_of_fall: bool,
    pub non_zero: bool,
    pub gate: bool,
}

const fn flags(end_of_rise: bool, end_of_fall: bool, non_zero: bool, gate: bool) -> AuxFlags {
    AuxFlags {
        end_of_rise,
        end_of_fall,
        non_zero,
        gate,
    }
}

pub fn aux_flags(env_mode: &EnvelopeMode) -> AuxFlags {
    match env_mode {
        EnvelopeMode::Adsr(phase) => match phase {
            AdsrState::Wait => flags(false, true, false, false),
            AdsrState::Attack => flags(false, false, true, true),
            AdsrState::Decay | AdsrState::Sustain => flags(true, false, true, true),
            AdsrState::Release => flags(true, false, true, false),
        },
        EnvelopeMode::Acrc(phase) => match phase {
            AcrcState::Wait => flags(false, true, false, false),
            AcrcState::Attack => flags(false, false, true, true),
            AcrcState::Hold => flags(true, false, true, true),
            AcrcState::Release => flags(true, false, true, false),
        },
        EnvelopeMode::AcrcLoop(phase) => match phase {
            AcrcLoopState::Attack => flags(false, true, true, false),
            AcrcLoopState::Release => flags(true, false, true, false),
        },
        EnvelopeMode::AhrdLoop(phase) => match phase {
            AhrdState::Attack => flags(false, false, true, false),
            AhrdState::Hold | AhrdState::Release => flags(true, false, true, false),
            AhrdState::Delay => flags(false, true, false, false),
        },
        // rising counts as the gate; settled counts as the end of both stages
        EnvelopeMode::Slew(s) => match s.phase {
            SlewPhase::Idle => flags(true, true, false, false),
            SlewPhase::Rising => flags(false, false, true, true),
            SlewPhase::Falling => flags(true, false, true, false),
            SlewPhase::Settled => flags(true, true, true, false),
        },
        // the end of each pulse's fall is an end of fall, so pulses can be counted
        EnvelopeMode::Burst(s) => match s.phase {
            BurstPhase::Idle => flags(false, true, false, false),
            BurstPhase::Rise => flags(false, s.pulse > 0, true, true),
            BurstPhase::Fall => flags(true, false, true, false),
        },
        EnvelopeMode::RandomLoop(s) => match s.phase {
            RandomPhase::Rise => flags(false, true, true, false),
            RandomPhase::Fall => flags(true, false, true, false),
        },
        // each impact is an end of fall
        EnvelopeMode::Ball(s) => match s.phase {
            BallPhase::Rest => flags(false, true, false, false),
            BallPhase::Lift | BallPhase::Held => flags(false, false, true, true),
            BallPhase::Down => flags(true, false, true, false),
            BallPhase::Up => flags(false, true, true, false),
        },
    }
}

/// About 5 ms (one less sample than this is high)
const PULSE_SAMPLES: u8 = 11;

pub struct AuxOutput {
    previous: AuxFlags,
    pulse_remaining: u8,
}

impl AuxOutput {
    pub const fn new() -> Self {
        Self {
            // no pulse for flags that are already set at startup
            previous: flags(true, true, true, true),
            pulse_remaining: 0,
        }
    }

    /// Whether a pulse is being output, so `update` must be called every sample
    pub fn pulsing(&self) -> bool {
        self.pulse_remaining > 0
    }

    /// Call whenever the stage changes and every sample while `pulsing`. Returns the
    /// aux output level.
    pub fn update(&mut self, flags: AuxFlags, mode: AuxMode) -> bool {
        let rose = |now: bool, before: bool| now && !before;
        let level = match mode {
            AuxMode::EndOfRise => flags.end_of_rise,
            AuxMode::EndOfFall => flags.end_of_fall,
            AuxMode::NonZero => flags.non_zero,
            AuxMode::FollowGate => flags.gate,
            AuxMode::EndOfRisePulse | AuxMode::EndOfFallPulse => {
                let edge = if mode == AuxMode::EndOfRisePulse {
                    rose(flags.end_of_rise, self.previous.end_of_rise)
                } else {
                    rose(flags.end_of_fall, self.previous.end_of_fall)
                };
                if edge {
                    self.pulse_remaining = PULSE_SAMPLES;
                }
                // the call that counts down to 0 turns the output off, so it can't be
                // left on once `pulsing` stops returning true
                self.pulse_remaining = self.pulse_remaining.saturating_sub(1);
                self.pulse_remaining > 0
            }
        };
        self.previous = flags;
        level
    }
}
