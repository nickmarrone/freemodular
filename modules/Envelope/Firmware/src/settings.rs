//! Hidden settings, edited by holding the button and turning a knob, and the knob
//! "pickup" that keeps a parameter from jumping after its knob was used for that.
//!
//! Hardware independent so it can be unit tested on the host.

use fixed::{types::extra::U16, FixedU16};

/// Raw ADC reading with the knob all the way up (the CV is inverted in hardware, so
/// the knob at minimum reads this and the knob at maximum reads 0)
pub const MAX_ADC_VALUE: u16 = 977;

/// Knob position from 0 (fully counter-clockwise) to MAX_ADC_VALUE
pub fn knob_position(raw_cv: u16) -> u16 {
    MAX_ADC_VALUE.saturating_sub(raw_cv)
}

/// What a new gate or trigger does to an envelope that is already running (ADSR and AR)
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum GateBehaviour {
    /// Restart the attack from the current level (no jump)
    Continue,
    /// Restart the attack from zero
    Reset,
    /// Ignore new gates and triggers while the envelope is still rising, decaying or
    /// sustaining; they only take over holding it open
    Legato,
    /// Loop the envelope for as long as the gate is held
    Cycle,
}

const GATE_BEHAVIOURS: [GateBehaviour; 4] = [
    GateBehaviour::Continue,
    GateBehaviour::Reset,
    GateBehaviour::Legato,
    GateBehaviour::Cycle,
];

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AuxMode {
    EndOfRise,
    EndOfFall,
    NonZero,
    FollowGate,
    /// A short trigger at the end of each rising stage
    EndOfRisePulse,
    /// A short trigger at the end of each falling stage
    EndOfFallPulse,
}

const AUX_MODES: [AuxMode; 6] = [
    AuxMode::EndOfRise,
    AuxMode::EndOfFall,
    AuxMode::NonZero,
    AuxMode::FollowGate,
    AuxMode::EndOfRisePulse,
    AuxMode::EndOfFallPulse,
];

/// LED pattern with LED `i` (0 = top) lit, in the format `UI::update` takes
pub const fn led(i: u8) -> u8 {
    1 << (4 + i)
}

/// Which option of `n` a knob position selects
fn option_index(position: u16, n: u16) -> usize {
    (position as u32 * n as u32 / (MAX_ADC_VALUE as u32 + 1)) as usize
}

/// A curve for `exp_curve`: (amount, bends the other way)
pub type Curve = (FixedU16<U16>, bool);

/// The settings the envelope modes use, in the form they need
#[derive(Copy, Clone)]
pub struct EnvelopeConfig {
    pub attack_curve: Curve,
    pub release_curve: Curve,
    pub gate_behaviour: GateBehaviour,
}

/// Curves are stored as knob position / 4; the middle of the knob is linear
const CURVE_CENTER: u16 = MAX_ADC_VALUE / 2;
const CURVE_DEADZONE: u16 = 16;
pub const LINEAR_CURVE: u8 = (CURVE_CENTER / 4) as u8;

/// Curve amount for `exp_curve`: (c, c_negative). Exactly zero (linear) around the
/// middle of the knob so the linear setting is easy to find.
pub fn curve_amount(stored: u8) -> Curve {
    let position = stored as u16 * 4;
    let (distance, negative) = if position >= CURVE_CENTER {
        (position - CURVE_CENTER, false)
    } else {
        (CURVE_CENTER - position, true)
    };
    if distance < CURVE_DEADZONE {
        return (FixedU16::ZERO, false);
    }
    const RANGE: u32 = (CURVE_CENTER - CURVE_DEADZONE) as u32;
    let bits = u32::min((distance - CURVE_DEADZONE) as u32 * 0xFFFF / RANGE, 0xFFFF);
    (FixedU16::<U16>::from_bits(bits as u16), negative)
}

fn curve_leds(stored: u8) -> u8 {
    let position = stored as u16 * 4;
    if position + CURVE_DEADZONE < CURVE_CENTER {
        if position < CURVE_CENTER / 2 {
            led(0)
        } else {
            led(1)
        }
    } else if position < CURVE_CENTER + CURVE_DEADZONE {
        led(1) | led(2)
    } else if position < CURVE_CENTER + CURVE_CENTER / 2 {
        led(2)
    } else {
        led(3)
    }
}

/// The saved byte holds the mode in the low bits and the time range in the top bit
pub const SAVED_LONG_RANGE_BIT: u8 = 0x80;
pub const SAVED_MODE_MASK: u8 = 0x07;
pub const SAVED_SIZE: usize = 6;
/// Marks a save written by firmware that knows about the hidden settings
const SAVED_MAGIC: u8 = 0xA5;
const AUX_FROM_JUMPERS: u8 = 0xFF;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    pub mode: u8,
    pub long_range: bool,
    /// ADSR attack curve (knob position / 4)
    pub attack_curve: u8,
    /// ADSR decay and release curve (knob position / 4)
    pub release_curve: u8,
    pub gate_behaviour: GateBehaviour,
    /// `None`: use the mode set by the jumpers
    pub aux_mode: Option<AuxMode>,
}

impl Settings {
    pub const DEFAULT: Settings = Settings {
        mode: 0,
        long_range: false,
        attack_curve: LINEAR_CURVE,
        release_curve: LINEAR_CURVE,
        gate_behaviour: GateBehaviour::Continue,
        aux_mode: None,
    };

    pub fn to_bytes(&self) -> [u8; SAVED_SIZE] {
        [
            self.mode | if self.long_range { SAVED_LONG_RANGE_BIT } else { 0 },
            SAVED_MAGIC,
            self.attack_curve,
            self.release_curve,
            self.gate_behaviour as u8,
            match self.aux_mode {
                Some(mode) => mode as u8,
                None => AUX_FROM_JUMPERS,
            },
        ]
    }

    /// Decodes a save, falling back to defaults for anything invalid. Saves from
    /// older firmware held a single byte (mode and range); the rest of what is read
    /// back is then unrelated data, so only the mode and range are kept.
    pub fn from_bytes(bytes: &[u8; SAVED_SIZE], num_modes: u8) -> Settings {
        let mut settings = Settings::DEFAULT;
        let mode = bytes[0] & SAVED_MODE_MASK;
        if mode < num_modes {
            settings.mode = mode;
        }
        settings.long_range = bytes[0] & SAVED_LONG_RANGE_BIT != 0;
        if bytes[1] != SAVED_MAGIC {
            return settings;
        }
        settings.attack_curve = bytes[2];
        settings.release_curve = bytes[3];
        if let Some(&g) = GATE_BEHAVIOURS.get(bytes[4] as usize) {
            settings.gate_behaviour = g;
        }
        settings.aux_mode = AUX_MODES.get(bytes[5] as usize).copied();
        settings
    }

    pub fn envelope_config(&self) -> EnvelopeConfig {
        EnvelopeConfig {
            attack_curve: curve_amount(self.attack_curve),
            release_curve: curve_amount(self.release_curve),
            gate_behaviour: self.gate_behaviour,
        }
    }

    /// Sets the hidden setting assigned to `knob` from that knob's raw reading
    pub fn set_from_knob(&mut self, knob: usize, cv: &[u16; 4]) {
        // (`& 3` and `get` keep bounds-check panics, which pull in all of core::fmt,
        // out of the firmware)
        let position = knob_position(cv[knob & 3]);
        match knob {
            0 => self.attack_curve = (position / 4) as u8,
            1 => self.release_curve = (position / 4) as u8,
            2 => {
                if let Some(&g) = GATE_BEHAVIOURS.get(option_index(position, 4)) {
                    self.gate_behaviour = g;
                }
            }
            _ => {
                if let Some(&a) = AUX_MODES.get(option_index(position, 6)) {
                    self.aux_mode = Some(a);
                }
            }
        }
    }

    /// LED pattern showing the hidden setting assigned to `knob`
    pub fn leds(&self, knob: usize, jumper_aux: AuxMode) -> u8 {
        match knob {
            0 => curve_leds(self.attack_curve),
            1 => curve_leds(self.release_curve),
            2 => led(self.gate_behaviour as u8),
            _ => match self.aux_mode.unwrap_or(jumper_aux) as u8 {
                4 => led(0) | led(1),
                5 => led(2) | led(3),
                i => led(i),
            },
        }
    }
}

/// How long the button must be held before turning a knob edits a hidden setting.
/// Quick clicks always change the mode, even with modulation patched into a knob.
pub const EDIT_ARM_MS: u32 = 250;
/// How far (in ADC steps) a knob must turn while the button is held to count as edited
pub const EDIT_THRESHOLD: u16 = 24;
/// How close a knob must come to its frozen value to pick it up again
pub const PICKUP_TOLERANCE: u16 = 8;

/// Tracks which knobs were turned while the button was held
pub struct Editor {
    snapshot: [u16; 4],
    armed: bool,
    edited: u8,
    current: Option<usize>,
}

impl Editor {
    pub const fn new() -> Self {
        Self {
            snapshot: [0; 4],
            armed: false,
            edited: 0,
            current: None,
        }
    }

    /// The button was just pressed
    pub fn press(&mut self) {
        self.armed = false;
        self.edited = 0;
        self.current = None;
    }

    /// Call while the button is held. Returns the knob whose setting is being edited.
    pub fn hold(&mut self, held_ms: u32, cv: &[u16; 4]) -> Option<usize> {
        if !self.armed {
            if held_ms >= EDIT_ARM_MS {
                self.snapshot = *cv;
                self.armed = true;
            }
            return None;
        }
        for i in 0..4 {
            if self.edited & (1 << i) == 0 && cv[i].abs_diff(self.snapshot[i]) > EDIT_THRESHOLD {
                self.edited |= 1 << i;
                self.current = Some(i);
            }
        }
        self.current
    }

    /// Keeps the envelope parameter of each knob being used to edit a setting at the
    /// value it had before
    pub fn freeze(&self, cv: &mut [u16; 4]) {
        for i in 0..4 {
            if self.edited & (1 << i) != 0 {
                cv[i] = self.snapshot[i];
            }
        }
    }

    /// The button was released. Hands the edited knobs over to `pickup`, and returns
    /// whether any setting was edited.
    pub fn release(&mut self, cv: &[u16; 4], pickup: &mut Pickup) -> bool {
        for i in 0..4 {
            if self.edited & (1 << i) != 0 {
                pickup.hold(i, self.snapshot[i], cv[i]);
            }
        }
        self.armed = false;
        self.current = None;
        core::mem::replace(&mut self.edited, 0) != 0
    }
}

/// Holds a knob's parameter at its old value until the knob is turned back through it
pub struct Pickup {
    frozen: [u16; 4],
    active: u8,
    above: u8,
}

impl Pickup {
    pub const fn new() -> Self {
        Self {
            frozen: [0; 4],
            active: 0,
            above: 0,
        }
    }

    fn hold(&mut self, knob: usize, value: u16, current: u16) {
        if current.abs_diff(value) <= PICKUP_TOLERANCE {
            self.active &= !(1 << knob);
            return;
        }
        self.frozen[knob] = value;
        self.active |= 1 << knob;
        if current > value {
            self.above |= 1 << knob;
        } else {
            self.above &= !(1 << knob);
        }
    }

    pub fn apply(&mut self, cv: &mut [u16; 4]) {
        if self.active == 0 {
            return;
        }
        for i in 0..4 {
            let bit = 1 << i;
            if self.active & bit == 0 {
                continue;
            }
            let frozen = self.frozen[i];
            let crossed = (cv[i] > frozen) != (self.above & bit != 0);
            if crossed || cv[i].abs_diff(frozen) <= PICKUP_TOLERANCE {
                self.active &= !bit;
            } else {
                cv[i] = frozen;
            }
        }
    }
}
