/*!
The clock engine is split in two halves:

- The *planner* (`apply_config`, `realign_channel`, ...) runs in the main loop. Whenever
  the config or tempo changes, it converts each channel's settings into a handful of
  integer constants (`ChannelParams`). All the slow math (divisions, u64) happens here.
- The *executor* (`Engine::tick`) runs in the TIMER1 compare interrupt at `TICK_HZ`.
  It only adds and compares integers, so output timing is independent of whatever the
  main loop (display, EEPROM) is doing.

Timekeeping uses phase accumulators. The master accumulator advances by `bpm10` every
tick and wraps at `MASTER_WRAP` (one beat). A channel running at `num/den` periods per
beat advances by `bpm10 * num` and wraps at `MASTER_WRAP * den`, so every channel stays
exactly phase locked to the master with no drift, and tempo changes are seamless.
*/

use core::ptr::addr_of_mut;

#[cfg(target_arch = "avr")]
use avr_device::interrupt;

pub const NUM_CHANNELS: usize = 8;

/// Executor tick rate. Output timing resolution is 1 / TICK_HZ = 125us.
const TICK_HZ: u32 = 8000;
/// Accumulator units in one beat. Tempo is in tenths of BPM, so this is 60s * 10.
const MASTER_WRAP: u32 = 60 * TICK_HZ * 10;
/// 5ms minimum trigger width / gap
const MIN_PULSE_TICKS: u32 = 5 * TICK_HZ / 1000;

pub const MIN_BPM10: u16 = 200;
pub const MAX_BPM10: u16 = 3000;

/// Output is high only while the clock is stopped
pub const DIV_STOP: i8 = -65;
/// Output is high only while the clock is running
pub const DIV_RUN: i8 = -66;
/// Output sends a trigger when the clock starts
pub const DIV_RESET: i8 = -67;
pub const MIN_DIVISION: i8 = DIV_RESET;
pub const MAX_DIVISION: i8 = 64;

pub const TUPLET_NONE: u8 = 0;
pub const TUPLET_TRIPLET: u8 = 1;
pub const TUPLET_DOTTED: u8 = 2;

pub const STOP_NOW: u8 = 0;
pub const STOP_BEAT: u8 = 1;
pub const STOP_BAR: u8 = 2;

pub const MAX_EUCLID_STEPS: u8 = 32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClockChannelConfig {
    /// x1 to x64 (positive), /2 to /64 (negative), or one of the special DIV_* values
    pub division: i8,
    pub swing: u8,
    pub pulse_width: u8,
    pub phase_shift: i8,
    pub tuplet: u8,
    pub probability: u8,
    /// 0 = Euclidean mode off
    pub euclid_steps: u8,
    pub euclid_fill: u8,
    pub euclid_rotate: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClockConfig {
    pub channels: [ClockChannelConfig; NUM_CHANNELS],
    /// Tempo in tenths of a BPM
    pub bpm10: u16,
    pub stop_mode: u8,
}

pub fn is_special_division(division: i8) -> bool {
    division < -64
}

impl ClockChannelConfig {
    #[inline(never)]
    pub fn is_valid(&self) -> bool {
        self.division >= MIN_DIVISION
            && self.division <= MAX_DIVISION
            && self.division != 0
            && self.division != -1
            && self.swing <= 32
            && self.pulse_width <= 100
            && self.phase_shift >= -32
            && self.phase_shift <= 32
            && self.tuplet <= TUPLET_DOTTED
            && self.probability <= 100
            && self.euclid_steps <= MAX_EUCLID_STEPS
            && self.euclid_steps != 1
            && self.euclid_fill <= self.euclid_steps
            && (self.euclid_rotate < self.euclid_steps || self.euclid_rotate == 0)
    }
}

impl ClockConfig {
    #[inline(never)]
    pub fn new() -> Self {
        const DEFAULT_DIVISIONS: [i8; 8] = [1, 2, 4, 8, -2, -4, -8, -16];
        ClockConfig {
            bpm10: 1280,
            channels: DEFAULT_DIVISIONS.map(|division| ClockChannelConfig {
                division,
                swing: 0,
                pulse_width: 50,
                phase_shift: 0,
                tuplet: TUPLET_NONE,
                probability: 100,
                euclid_steps: 0,
                euclid_fill: 0,
                euclid_rotate: 0,
            }),
            stop_mode: STOP_NOW,
        }
    }

    #[inline(never)]
    pub fn is_valid(&self) -> bool {
        self.bpm10 >= MIN_BPM10
            && self.bpm10 <= MAX_BPM10
            && self.stop_mode <= STOP_BAR
            && (0..core::hint::black_box(NUM_CHANNELS)).all(|i| self.channels[i].is_valid())
    }
}

const KIND_NORMAL: u8 = 0;
const KIND_STOP: u8 = 1;
const KIND_RUN: u8 = 2;
const KIND_RESET: u8 = 3;

/// Precomputed per-channel constants, all in accumulator units. Only needed when
/// a channel reaches an event, not on every tick.
#[derive(Clone, Copy)]
struct ChannelParams {
    wrap: u32,
    high: u32,
    swing_start: u32,
    swing_end: u32,
    /// probability threshold out of 256 (256 = always)
    prob: u16,
    steps: u8,
    fill: u8,
    kind: u8,
}

/// Each period is a sequence of three events: the pulse starts, the pulse ends, and
/// the period wraps around.
const STAGE_ON: u8 = 0;
const STAGE_OFF: u8 = 1;
const STAGE_WRAP: u8 = 2;

#[derive(Clone, Copy)]
struct ChannelState {
    // The hot path of the interrupt only touches these three fields: it adds `inc`
    // to `acc` and checks whether `acc` has reached `next`, the position of the next
    // event. Everything else happens a few times per period at most.
    acc: u32,
    next: u32,
    inc: u32,
    stage: u8,
    odd: bool,
    /// Euclidean (Bresenham) bucket for the *next* period
    bucket: u8,
    /// whether the current period fires (Euclidean hit and probability roll)
    active: bool,
}

pub struct Engine {
    params: [ChannelParams; NUM_CHANNELS],
    state: [ChannelState; NUM_CHANNELS],
    master_acc: u32,
    beat: u32,
    bpm10: u16,
    running: bool,
    /// 0 = no stop pending; otherwise stop on the next beat divisible by this (power of 2)
    stop_div: u8,
    /// output bits of the normal (clock) channels
    normal_out: u8,
    stop_mask: u8,
    run_mask: u8,
    reset_mask: u8,
    /// keeps outputs that were high when the clock stopped high long enough to be a
    /// valid trigger (avoids runt pulses)
    hold_bits: u8,
    hold_ticks: u8,
    reset_ticks: u8,
    out: u8,
    beat_flag: bool,
    rng: u16,
    sub_ms: u8,
    millis: u32,
}

// Everything starts zeroed so the engine lives in .bss instead of taking up flash for
// an initializer in .data. The engine isn't started until `apply_config` and `start`.
const ZERO_PARAMS: ChannelParams = ChannelParams {
    wrap: 0,
    high: 0,
    swing_start: 0,
    swing_end: 0,
    prob: 0,
    steps: 0,
    fill: 0,
    kind: KIND_NORMAL,
};
const ZERO_STATE: ChannelState = ChannelState {
    acc: 0,
    next: 0,
    inc: 0,
    stage: STAGE_ON,
    odd: false,
    bucket: 0,
    active: false,
};

static mut ENGINE: Engine = Engine {
    params: [ZERO_PARAMS; NUM_CHANNELS],
    state: [ZERO_STATE; NUM_CHANNELS],
    master_acc: 0,
    beat: 0,
    bpm10: 0,
    running: false,
    stop_div: 0,
    normal_out: 0,
    stop_mask: 0,
    run_mask: 0,
    reset_mask: 0,
    hold_bits: 0,
    hold_ticks: 0,
    reset_ticks: 0,
    out: 0,
    beat_flag: false,
    rng: 0,
    sub_ms: 0,
    millis: 0,
};

#[cfg(target_arch = "avr")]
#[inline(always)]
fn with_engine<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    interrupt::free(|_| f(unsafe { &mut *addr_of_mut!(ENGINE) }))
}

#[cfg(target_arch = "avr")]
#[avr_device::interrupt(atmega328p)]
fn TIMER1_COMPA() {
    unsafe { (*addr_of_mut!(ENGINE)).tick() }
}

/// Host-side test hooks (the engine logic is unit tested off-target)
#[cfg(not(target_arch = "avr"))]
fn with_engine<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    f(unsafe { &mut *addr_of_mut!(ENGINE) })
}

/// Run one executor tick and return the output bits
#[cfg(not(target_arch = "avr"))]
pub fn test_tick() -> u8 {
    with_engine(|e| {
        e.tick();
        e.out
    })
}

/// Position within the period of the event a channel is waiting for
#[inline(never)]
fn threshold(p: &ChannelParams, odd: bool, stage: u8) -> u32 {
    match (stage, odd) {
        (STAGE_ON, false) => 0,
        (STAGE_ON, true) => p.swing_start,
        (STAGE_OFF, false) => p.high,
        (STAGE_OFF, true) => p.swing_end,
        _ => p.wrap,
    }
}

impl Engine {
    #[inline(always)]
    fn tick(&mut self) {
        // Write the value computed last tick first so output latency is constant
        #[cfg(target_arch = "avr")]
        let portd = unsafe { &*avr_device::atmega328p::PORTD::ptr() };
        #[cfg(target_arch = "avr")]
        portd.portd.write(|w| unsafe { w.bits(self.out) });

        self.sub_ms += 1;
        if self.sub_ms == (TICK_HZ / 1000) as u8 {
            self.sub_ms = 0;
            self.millis = self.millis.wrapping_add(1);
        }

        let running = self.running;
        if running {
            // Each channel's output is determined by its position at the start of
            // this tick, then the position advances. Special channels have inc = 0
            // and next = MAX so they never trigger events.
            for i in 0..NUM_CHANNELS {
                let s = unsafe { self.state.get_unchecked(i) };
                if s.acc >= s.next {
                    self.process_events(i);
                }
                let s = unsafe { self.state.get_unchecked_mut(i) };
                s.acc += s.inc;
            }
        }

        let mut out = self.normal_out
            | if running {
                self.run_mask
            } else {
                self.stop_mask
            };
        if self.hold_ticks > 0 {
            out |= self.hold_bits;
            self.hold_ticks -= 1;
        }
        if self.reset_ticks > 0 {
            out |= self.reset_mask;
            self.reset_ticks -= 1;
        }
        self.out = out;

        if running {
            self.master_acc += self.bpm10 as u32;
            if self.master_acc >= MASTER_WRAP {
                self.master_acc -= MASTER_WRAP;
                self.beat = self.beat.wrapping_add(1);
                self.beat_flag = true;
                if self.stop_div != 0 && (self.beat as u8) & (self.stop_div - 1) == 0 {
                    // stop exactly on the beat/bar line, before its pulses start
                    self.halt();
                }
            }
        }
    }

    /// Handle every event a channel has reached. Runs a few times per period.
    #[inline(never)]
    fn process_events(&mut self, i: usize) {
        let p = &self.params[i];
        let s = &mut self.state[i];
        let bit = 1u8 << i;
        loop {
            match s.stage {
                STAGE_ON => {
                    if s.active {
                        self.normal_out |= bit;
                    }
                    s.stage = STAGE_OFF;
                }
                STAGE_OFF => {
                    self.normal_out &= !bit;
                    s.stage = STAGE_WRAP;
                }
                _ => {
                    s.acc -= p.wrap;
                    s.odd = !s.odd;
                    let mut hit = true;
                    if p.steps != 0 {
                        hit = s.bucket < p.fill;
                        s.bucket += p.fill;
                        if s.bucket >= p.steps {
                            s.bucket -= p.steps;
                        }
                    }
                    if p.prob < 256 {
                        let mut x = self.rng;
                        x ^= x << 7;
                        x ^= x >> 9;
                        x ^= x << 8;
                        self.rng = x;
                        hit &= (x >> 8) < p.prob;
                    }
                    s.active = hit;
                    s.stage = STAGE_ON;
                }
            }
            s.next = threshold(p, s.odd, s.stage);
            if s.acc < s.next {
                break;
            }
        }
    }

    fn halt(&mut self) {
        self.hold_bits = self.normal_out;
        self.hold_ticks = MIN_PULSE_TICKS as u8;
        self.normal_out = 0;
        self.running = false;
        self.stop_div = 0;
    }
}

/// Configure TIMER1 to fire the executor interrupt at TICK_HZ
#[cfg(target_arch = "avr")]
pub fn init_timer(tc1: avr_device::atmega328p::TC1) {
    tc1.tccr1a.write(|w| unsafe { w.bits(0) });
    // CTC mode (WGM12), prescaler 8 -> 2MHz
    tc1.tccr1b.write(|w| w.wgm1().bits(0b01).cs1().prescale_8());
    tc1.ocr1a.write(|w| w.bits((16_000_000 / 8 / TICK_HZ - 1) as u16));
    tc1.timsk1.write(|w| w.ocie1a().set_bit());
}

/// Milliseconds since boot (wraps after ~49 days, so compare with wrapping_sub)
pub fn millis() -> u32 {
    with_engine(|e| e.millis)
}

/// Returns true once per beat, used to animate the screensaver
pub fn take_beat_flag() -> bool {
    with_engine(|e| core::mem::replace(&mut e.beat_flag, false))
}

pub fn is_running() -> bool {
    with_engine(|e| e.running)
}

pub fn stop_is_pending() -> bool {
    with_engine(|e| e.stop_div != 0)
}

/// Rate of a channel in periods per beat, as a fraction
fn channel_ratio(channel: &ClockChannelConfig) -> (u16, u16) {
    let (mut num, mut den) = if channel.division > 0 {
        (channel.division as u16, 1u16)
    } else {
        (1u16, (-(channel.division as i16)) as u16)
    };
    match channel.tuplet {
        TUPLET_TRIPLET => {
            num *= 3;
            den *= 2;
        }
        TUPLET_DOTTED => {
            num *= 2;
            den *= 3;
        }
        _ => {}
    }
    (num, den)
}

#[inline(never)]
fn compute_params(channel: &ClockChannelConfig, bpm10: u16) -> ChannelParams {
    let kind = match channel.division {
        DIV_STOP => KIND_STOP,
        DIV_RUN => KIND_RUN,
        DIV_RESET => KIND_RESET,
        _ => KIND_NORMAL,
    };
    if kind != KIND_NORMAL {
        return ChannelParams {
            kind,
            prob: 256,
            ..ZERO_PARAMS
        };
    }
    let (num, den) = channel_ratio(channel);
    let inc = bpm10 as u32 * num as u32;
    let wrap = MASTER_WRAP * den as u32;
    let min = MIN_PULSE_TICKS * inc;
    let max_pw = wrap.saturating_sub(min);
    let tiny = min >= max_pw;
    let high = if tiny {
        // If the period gets very small, ignore pulse width
        wrap / 2
    } else {
        match channel.pulse_width {
            0 => min,
            100 => max_pw,
            pw => (wrap / 100 * pw as u32).clamp(min, max_pw),
        }
    };
    let swing_start = wrap / 64 * channel.swing as u32;
    // Consecutive gates are always separated by at least the minimum gap
    let swing_end = (swing_start + high).min(if tiny { wrap } else { max_pw });
    ChannelParams {
        wrap,
        high,
        swing_start,
        swing_end,
        prob: channel.probability as u16 * 256 / 100,
        steps: channel.euclid_steps,
        fill: channel.euclid_fill,
        kind,
    }
}

/// Recompute the executor constants for every channel. Call whenever any setting
/// changes. Channel positions are kept, so this alone is enough for tempo, pulse
/// width, swing, and probability changes. Channels in `realign_mask` (whose rate,
/// phase, or Euclidean pattern changed) are also moved to where they would be had
/// they been running with the new settings since the clock started, so they line up
/// with the master grid.
#[inline(never)]
pub fn apply_config(config: &ClockConfig, realign_mask: u8) {
    let mut params = [ZERO_PARAMS; NUM_CHANNELS];
    let mut incs = [0u32; NUM_CHANNELS];
    let mut masks = [0u8; 4];
    for i in 0..core::hint::black_box(NUM_CHANNELS) {
        params[i] = compute_params(&config.channels[i], config.bpm10);
        masks[params[i].kind as usize] |= 1 << i;
        incs[i] = config.bpm10 as u32 * channel_ratio(&config.channels[i]).0 as u32;
    }

    // Publish the tempo for the master and every channel at the same instant so
    // nothing drifts relative to anything else
    with_engine(|e| {
        e.bpm10 = config.bpm10;
        e.stop_mask = masks[KIND_STOP as usize];
        e.run_mask = masks[KIND_RUN as usize];
        e.reset_mask = masks[KIND_RESET as usize];
        for i in 0..core::hint::black_box(NUM_CHANNELS) {
            let p = &params[i];
            let s = &mut e.state[i];
            if p.kind != KIND_NORMAL {
                *s = ChannelState {
                    next: u32::MAX,
                    ..ZERO_STATE
                };
                e.normal_out &= !(1 << i);
            } else if realign_mask & (1 << i) == 0 {
                s.inc = incs[i];
                s.next = threshold(p, s.odd, s.stage);
            } else {
                // keep running on the old settings until realigned below
                continue;
            }
            e.params[i] = *p;
        }
    });

    for i in 0..core::hint::black_box(NUM_CHANNELS) {
        if realign_mask & (1 << i) != 0 && params[i].kind == KIND_NORMAL {
            realign_channel(&config.channels[i], i, &params[i], incs[i]);
        }
    }
}

#[inline(never)]
fn realign_channel(channel: &ClockChannelConfig, i: usize, params: &ChannelParams, inc: u32) {
    let (num, _) = channel_ratio(channel);
    let (beat, master_acc) = with_engine(|e| (e.beat, e.master_acc));

    // The slow u64 math happens outside the critical section so ticks aren't missed
    let wrap = params.wrap as u64;
    let mut pos = (beat as u64 * MASTER_WRAP as u64 + master_acc as u64) * num as u64;
    let shift = wrap * channel.phase_shift.unsigned_abs() as u64 / 64;
    if channel.phase_shift > 0 {
        // delayed: before the first period starts, act as if we're at the end of
        // the previous pattern repetition
        if pos < shift {
            pos += wrap * 2 * channel.euclid_steps.max(1) as u64;
        }
        pos -= shift;
    } else {
        pos += shift;
    }
    let period_idx = pos / wrap;
    let mut state = ChannelState {
        acc: (pos - period_idx * wrap) as u32,
        next: 0,
        inc,
        stage: STAGE_ON,
        odd: period_idx & 1 == 1,
        bucket: 0,
        active: true,
    };
    let steps = params.steps;
    if steps != 0 {
        let fill = params.fill as u16;
        let step = ((period_idx + channel.euclid_rotate as u64) % steps as u64) as u16;
        state.active = (step * fill) % (steps as u16) < fill;
        state.bucket = (((step + 1) * fill) % steps as u16) as u8;
    }

    with_engine(|e| {
        // catch up on the ticks that happened while calculating
        let elapsed = e
            .beat
            .wrapping_sub(beat)
            .wrapping_mul(MASTER_WRAP)
            .wrapping_add(e.master_acc)
            .wrapping_sub(master_acc);
        state.acc += elapsed * num as u32;
        while state.acc >= params.wrap {
            state.acc -= params.wrap;
            state.odd = !state.odd;
            if steps != 0 {
                state.active = state.bucket < params.fill;
                state.bucket += params.fill;
                if state.bucket >= steps {
                    state.bucket -= steps;
                }
            }
        }

        let bit = 1u8 << i;
        let on_at = threshold(params, state.odd, STAGE_ON);
        let off_at = threshold(params, state.odd, STAGE_OFF);
        if state.acc < on_at {
            state.stage = STAGE_ON;
            e.normal_out &= !bit;
        } else if state.acc < off_at {
            // Only keep a pulse going if it was already high (or is just starting);
            // jumping into the middle of a pulse would create a spurious trigger
            let keep = e.normal_out & bit != 0 || state.acc < on_at + inc || !e.running;
            if !(keep && state.active) {
                e.normal_out &= !bit;
            } else {
                e.normal_out |= bit;
            }
            state.stage = STAGE_OFF;
        } else {
            state.stage = STAGE_WRAP;
            e.normal_out &= !bit;
        }
        state.next = threshold(params, state.odd, state.stage);
        e.params[i] = *params;
        e.state[i] = state;
    });
}

/// Start the clock from the first beat
#[inline(never)]
pub fn start(config: &ClockConfig) {
    with_engine(|e| {
        e.running = false;
        e.normal_out = 0;
        e.master_acc = 0;
        e.beat = 0;
        e.stop_div = 0;
        e.hold_ticks = 0;
        e.reset_ticks = MIN_PULSE_TICKS as u8;
        e.rng ^= e.millis as u16;
        if e.rng == 0 {
            e.rng = 0xACE1;
        }
    });
    apply_config(config, 0xff);
    with_engine(|e| e.running = true);
}

/// Stop the clock according to `stop_mode`: immediately, or on the next beat/bar
#[inline(never)]
pub fn stop(stop_mode: u8) {
    with_engine(|e| match stop_mode {
        STOP_BEAT => e.stop_div = 1,
        STOP_BAR => e.stop_div = 4,
        _ => e.halt(),
    })
}

/// Cancel a pending quantized stop
pub fn cancel_stop() {
    with_engine(|e| e.stop_div = 0)
}
