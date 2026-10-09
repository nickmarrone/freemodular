mod acrc;
mod adsr;
mod ahrd;
mod shared;

use acrc::{acrc, acrc_loop};
use adsr::adsr;
use ahrd::ahrd;

pub use self::acrc::{AcrcLoopState, AcrcState};
pub use self::adsr::AdsrState;
pub use self::ahrd::AhrdState;
pub use self::shared::set_long_time_range;
use crate::settings::{led, EnvelopeConfig};

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum GateState {
    Rising,
    Falling,
    High,
    Low,
}

pub struct Input {
    pub gate: GateState,
    pub trigger: bool,
}

pub struct EnvelopeState {
    pub mode: EnvelopeMode,
    pub time: u32,
    pub last_value: u16,
    pub artificial_gate: bool,
}

#[derive(Copy, Clone)]
pub enum EnvelopeMode {
    Adsr(AdsrState),
    Acrc(AcrcState),
    AcrcLoop(AcrcLoopState),
    AhrdLoop(AhrdState),
}

impl EnvelopeMode {
    pub const COUNT: u8 = 4;

    pub const fn index(&self) -> u8 {
        match self {
            EnvelopeMode::Adsr(_) => 0,
            EnvelopeMode::Acrc(_) => 1,
            EnvelopeMode::AcrcLoop(_) => 2,
            EnvelopeMode::AhrdLoop(_) => 3,
        }
    }

    pub fn from_index(index: u8) -> Self {
        match index {
            1 => EnvelopeMode::Acrc(AcrcState::default()),
            2 => EnvelopeMode::AcrcLoop(AcrcLoopState::default()),
            3 => EnvelopeMode::AhrdLoop(AhrdState::default()),
            _ => EnvelopeMode::Adsr(AdsrState::default()),
        }
    }

    pub fn next(&self) -> Self {
        Self::from_index((self.index() + 1) % Self::COUNT)
    }
}

/// Modes 1-4 light one LED; modes 5-8 light all but one
pub const fn ui_show_mode(state: &EnvelopeMode) -> u8 {
    let index = state.index();
    if index < 4 {
        led(index)
    } else {
        !led(index - 4) & 0xF0
    }
}

pub const fn ui_show_stage(state: &EnvelopeMode) -> u8 {
    match state {
        EnvelopeMode::Adsr(phase) => match phase {
            AdsrState::Wait => 0b0000 as u8,
            AdsrState::Attack => 0b1000,
            AdsrState::Decay => 0b0100,
            AdsrState::Sustain => 0b0010,
            AdsrState::Release => 0b0001,
        },
        EnvelopeMode::Acrc(phase) => match phase {
            AcrcState::Wait => 0b0000,
            AcrcState::Attack => 0b1100,
            AcrcState::Hold => 0b0000,
            AcrcState::Release => 0b0011,
        },
        EnvelopeMode::AcrcLoop(phase) => match phase {
            AcrcLoopState::Attack => 0b1100,
            AcrcLoopState::Release => 0b0011,
        },
        EnvelopeMode::AhrdLoop(phase) => match phase {
            AhrdState::Attack => 0b1000,
            AhrdState::Hold => 0b0100,
            AhrdState::Release => 0b0010,
            AhrdState::Delay => 0b0001,
        },
    }
    .reverse_bits()
}

pub fn update(
    state: &mut EnvelopeState,
    input: &Input,
    cv: &[u16; 4],
    config: &EnvelopeConfig,
) -> (u16, bool) {
    let (value, rollover) = match state.mode {
        EnvelopeMode::Adsr(ref mut phase) => adsr(
            phase,
            &mut state.time,
            state.last_value,
            input,
            cv,
            &mut state.artificial_gate,
            config,
        ),
        EnvelopeMode::Acrc(ref mut phase) => acrc(
            phase,
            &mut state.time,
            state.last_value,
            input,
            cv,
            &mut state.artificial_gate,
            config,
        ),
        EnvelopeMode::AcrcLoop(ref mut phase) => acrc_loop(phase, &mut state.time, input, cv),
        EnvelopeMode::AhrdLoop(ref mut phase) => ahrd(phase, &mut state.time, input, cv),
    };

    debug_assert!(value <= MAX_DAC_VALUE);
    state.last_value = value;

    (value, rollover)
}

const MAX_DAC_VALUE: u16 = 4095;
