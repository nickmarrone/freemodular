mod acrc;
mod adsr;
mod ahrd;
mod ball;
mod burst;
mod random;
mod shared;
mod slew;

use acrc::{acrc, acrc_loop};
use adsr::adsr;
use ahrd::ahrd;
use ball::ball;
use burst::burst;
use random::random_loop;
use slew::slew;

pub use self::acrc::{AcrcLoopState, AcrcState};
pub use self::adsr::AdsrState;
pub use self::ahrd::AhrdState;
pub use self::ball::{BallPhase, BallState};
pub use self::burst::{BurstPhase, BurstState};
pub use self::random::{RandomPhase, RandomState};
pub use self::slew::{SlewPhase, SlewState};
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
    Slew(SlewState),
    Burst(BurstState),
    RandomLoop(RandomState),
    Ball(BallState),
}

impl EnvelopeMode {
    pub const COUNT: u8 = 8;

    pub const fn index(&self) -> u8 {
        match self {
            EnvelopeMode::Adsr(_) => 0,
            EnvelopeMode::Acrc(_) => 1,
            EnvelopeMode::AcrcLoop(_) => 2,
            EnvelopeMode::AhrdLoop(_) => 3,
            EnvelopeMode::Slew(_) => 4,
            EnvelopeMode::Burst(_) => 5,
            EnvelopeMode::RandomLoop(_) => 6,
            EnvelopeMode::Ball(_) => 7,
        }
    }

    pub fn from_index(index: u8) -> Self {
        match index {
            1 => EnvelopeMode::Acrc(AcrcState::default()),
            2 => EnvelopeMode::AcrcLoop(AcrcLoopState::default()),
            3 => EnvelopeMode::AhrdLoop(AhrdState::default()),
            4 => EnvelopeMode::Slew(SlewState::default()),
            5 => EnvelopeMode::Burst(BurstState::default()),
            6 => EnvelopeMode::RandomLoop(RandomState::default()),
            7 => EnvelopeMode::Ball(BallState::default()),
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
    // written with the top LED as the leftmost bit
    let pattern: u8 = match state {
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
        EnvelopeMode::Slew(s) => match s.phase {
            SlewPhase::Idle => 0b0000,
            SlewPhase::Rising => 0b1100,
            SlewPhase::Falling => 0b0011,
            SlewPhase::Settled => 0b0110,
        },
        // one LED per pulse or bounce, moving down
        EnvelopeMode::Burst(s) => match s.phase {
            BurstPhase::Idle => 0b0000,
            BurstPhase::Rise | BurstPhase::Fall => 0b1000 >> (s.pulse % 4),
        },
        EnvelopeMode::RandomLoop(s) => match s.phase {
            RandomPhase::Rise => 0b1100,
            RandomPhase::Fall => 0b0011,
        },
        EnvelopeMode::Ball(s) => match s.phase {
            BallPhase::Rest => 0b0000,
            BallPhase::Lift | BallPhase::Held => 0b1111,
            BallPhase::Down | BallPhase::Up => 0b1000 >> (s.bounces % 4),
        },
    };
    pattern.reverse_bits()
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
        EnvelopeMode::Slew(ref mut s) => slew(s, input, cv),
        EnvelopeMode::Burst(ref mut s) => burst(s, &mut state.time, input, cv),
        EnvelopeMode::RandomLoop(ref mut s) => random_loop(s, &mut state.time, input, cv),
        EnvelopeMode::Ball(ref mut s) => ball(s, &mut state.time, state.last_value, input, cv),
    };

    debug_assert!(value <= MAX_DAC_VALUE);
    state.last_value = value;

    (value, rollover)
}

const MAX_DAC_VALUE: u16 = 4095;
