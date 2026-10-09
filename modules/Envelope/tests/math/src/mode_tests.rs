//! Drives the envelope modes sample by sample, as the firmware's main loop does

use crate::envelope::*;
use crate::settings::*;

/// Raw ADC reading for a knob position in permille (the CV is inverted in hardware)
fn raw(permille: u32) -> u16 {
    MAX_ADC_VALUE - (permille * MAX_ADC_VALUE as u32 / 1000) as u16
}

struct Sim {
    state: EnvelopeState,
    config: EnvelopeConfig,
    cv: [u16; 4],
    gate_was_high: bool,
}

impl Sim {
    fn new(mode: u8, knobs: [u32; 4]) -> Self {
        Self::with_settings(mode, knobs, Settings::DEFAULT)
    }

    fn with_settings(mode: u8, knobs: [u32; 4], settings: Settings) -> Self {
        Sim {
            state: EnvelopeState {
                mode: EnvelopeMode::from_index(mode),
                time: 0,
                last_value: 0,
                artificial_gate: false,
            },
            config: settings.envelope_config(),
            cv: knobs.map(raw),
            gate_was_high: false,
        }
    }

    fn knob(&mut self, i: usize, permille: u32) {
        self.cv[i] = raw(permille);
    }

    fn step(&mut self, gate: bool, trigger: bool) -> u16 {
        let gate_state = match (self.gate_was_high, gate) {
            (true, true) => GateState::High,
            (true, false) => GateState::Falling,
            (false, true) => GateState::Rising,
            (false, false) => GateState::Low,
        };
        self.gate_was_high = gate;
        let input = Input {
            gate: gate_state,
            trigger,
        };
        update(&mut self.state, &input, &self.cv, &self.config).0
    }

    fn run(&mut self, samples: usize, gate: bool) -> Vec<u16> {
        (0..samples).map(|_| self.step(gate, false)).collect()
    }

    fn trigger(&mut self) -> u16 {
        self.step(false, true)
    }
}

/// Samples per full stage for a time knob position (knob response in shared.rs)
fn stage_samples(permille: u32) -> u32 {
    let x = (MAX_ADC_VALUE as u32 * permille / 1000) as u32;
    let numerator = if x < 512 {
        x / 4
    } else if x < 768 {
        x - 384
    } else {
        3 * x - 1920
    };
    (numerator * (10_000_000 / 480)) >> 10
}

/// Local maxima of a signal (a flat top counts once)
fn peaks(values: &[u16]) -> Vec<(usize, u16)> {
    let mut result = vec![];
    for i in 1..values.len() - 1 {
        if values[i] > values[i - 1] && values[i] > 0 {
            let end = (i..values.len()).find(|&j| values[j] != values[i]).unwrap_or(values.len());
            if end == values.len() || values[end] < values[i] {
                result.push((i, values[i]));
            }
        }
    }
    result
}

fn max_jump(values: &[u16]) -> u16 {
    values.windows(2).map(|w| w[0].abs_diff(w[1])).max().unwrap()
}

const ADSR: u8 = 0;
const AR: u8 = 1;
const SLEW: u8 = 4;
const BURST: u8 = 5;
const RANDOM: u8 = 6;
const BALL: u8 = 7;

#[test]
fn slew_reaches_the_input_in_the_rise_time() {
    // input full, rise 500 (linear shape)
    let mut sim = Sim::new(SLEW, [1000, 500, 500, 0]);
    let out = sim.run(5000, false);
    let arrived = out.iter().position(|&v| v == 4095).unwrap() as u32;
    let expected = stage_samples(500);
    assert!(arrived.abs_diff(expected) < expected / 50, "{arrived} vs {expected}");
    // monotonic, no overshoot
    assert!(out.windows(2).all(|w| w[1] >= w[0]));
    assert!(matches!(sim.state.mode, EnvelopeMode::Slew(SlewState { phase: SlewPhase::Settled, .. })));
}

#[test]
fn slew_exponential_shape_arrives_and_is_faster_at_first() {
    let mut linear = Sim::new(SLEW, [1000, 500, 500, 0]);
    let mut expo = Sim::new(SLEW, [1000, 500, 500, 1000]);
    let a = linear.run(8000, false);
    let b = expo.run(8000, false);
    assert!(b[300] > a[300] + 500, "{} vs {}", b[300], a[300]);
    assert_eq!(*b.last().unwrap(), 4095);
    assert!(b.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn slew_gate_holds_and_trigger_drops() {
    let mut sim = Sim::new(SLEW, [500, 300, 300, 0]);
    sim.run(4000, false);
    let settled = sim.state.last_value;
    assert!(settled.abs_diff(2048) < 20, "{settled}");
    sim.knob(0, 1000);
    let held = sim.run(500, true);
    assert!(held.iter().all(|&v| v == settled));
    // released: moves up again
    let moving = sim.run(50, false);
    assert!(moving[49] > settled);
    // back to zero, then on its way up again
    assert!(sim.trigger() < 20);
}

#[test]
fn burst_fires_the_set_number_of_pulses() {
    for (count_knob, count) in [(0, 1), (300, 5), (1000, 16)] {
        // short pulses, flat heights, linear-ish curve
        let mut sim = Sim::new(BURST, [100, count_knob, 500, 500]);
        sim.trigger();
        let out = sim.run(20_000, false);
        let p = peaks(&out);
        assert_eq!(p.len(), count, "knob {count_knob}");
        assert!(p.iter().all(|&(_, v)| v > 4000));
        assert_eq!(*out.last().unwrap(), 0);
        assert!(matches!(sim.state.mode, EnvelopeMode::Burst(BurstState { phase: BurstPhase::Idle, .. })));
    }
}

#[test]
fn burst_heights_shrink_or_grow() {
    let mut shrink = Sim::new(BURST, [100, 300, 0, 500]);
    shrink.trigger();
    let p: Vec<u16> = peaks(&shrink.run(20_000, false)).iter().map(|p| p.1).collect();
    assert_eq!(p.len(), 5);
    assert!(p.windows(2).all(|w| w[1] < w[0]), "{p:?}");
    assert!(p[4] < p[0] / 8, "{p:?}");

    let mut grow = Sim::new(BURST, [100, 300, 1000, 500]);
    grow.trigger();
    let p: Vec<u16> = peaks(&grow.run(20_000, false)).iter().map(|p| p.1).collect();
    assert!(p.windows(2).all(|w| w[1] > w[0]), "{p:?}");
    assert!(p[4] > 4000);
}

#[test]
fn burst_repeats_while_the_gate_is_held() {
    let mut sim = Sim::new(BURST, [100, 0, 500, 500]);
    let out = sim.run(stage_samples(100) as usize * 5, true);
    assert!(peaks(&out).len() >= 4);
}

#[test]
fn random_loop_without_randomness_repeats_exactly() {
    let mut sim = Sim::new(RANDOM, [100, 0, 0, 500]);
    let out = sim.run(5000, false);
    let p = peaks(&out);
    assert!(p.len() >= 5);
    assert!(p.iter().all(|&(_, v)| v > 4080));
    let gaps: Vec<usize> = p.windows(2).map(|w| w[1].0 - w[0].0).collect();
    assert!(gaps.iter().all(|&g| g.abs_diff(gaps[0]) <= 1), "{gaps:?}");
}

#[test]
fn random_loop_varies_within_its_range() {
    let mut sim = Sim::new(RANDOM, [100, 1000, 500, 500]);
    let out = sim.run(200_000, false);
    let p = peaks(&out);
    let heights: Vec<u16> = p.iter().map(|p| p.1).collect();
    let gaps: Vec<usize> = p.windows(2).map(|w| w[1].0 - w[0].0).collect();
    let (lo, hi) = (*heights.iter().min().unwrap(), *heights.iter().max().unwrap());
    // half height randomness: peaks between ~50% and 100%
    assert!(lo >= 2000 && lo < 2400 && hi > 3900, "{lo} {hi}");
    // time randomness up to 2 octaves either way, per stage
    let nominal = 2 * stage_samples(100) as usize;
    let (glo, ghi) = (*gaps.iter().min().unwrap(), *gaps.iter().max().unwrap());
    assert!(glo * 4 >= nominal / 2 && glo < nominal / 2, "{glo} {nominal}");
    assert!(ghi <= nominal * 4 + 4 && ghi > nominal * 2, "{ghi} {nominal}");
    // no jumps bigger than a fast stage would make
    assert!(max_jump(&out) < 600, "{}", max_jump(&out));
}

#[test]
fn random_loop_pauses_at_zero_while_gated() {
    let mut sim = Sim::new(RANDOM, [100, 500, 500, 500]);
    let out = sim.run(3000, true);
    let first_zero = out.iter().skip(10).position(|&v| v == 0).unwrap() + 10;
    assert!(out[first_zero..].iter().all(|&v| v == 0));
}

#[test]
fn ball_bounces_lower_and_quicker_then_rests() {
    // drop 250, bounciness 800 (e ~= 0.75), parabolic, full height
    let mut sim = Sim::new(BALL, [250, 800, 500, 1000]);
    // drops from the top: the first sample is already a little way down
    assert!(sim.trigger() > 4090);
    let out = sim.run(30_000, false);
    let p = peaks(&out);
    assert!(p.len() >= 8, "{}", p.len());
    let e = (MAX_ADC_VALUE as f64 * 0.8).round() * 63.0 / 65536.0;
    for w in p.windows(2).take(6) {
        let ratio = w[1].1 as f64 / w[0].1 as f64;
        assert!((ratio - e * e).abs() < 0.03, "height ratio {ratio} vs {}", e * e);
    }
    // impacts (the output touches zero and leaves it) get closer by e
    let impacts: Vec<usize> = (0..out.len() - 1).filter(|&i| out[i] == 0 && out[i + 1] > 0).collect();
    for w in impacts.windows(3).take(4) {
        let ratio = (w[2] - w[1]) as f64 / (w[1] - w[0]) as f64;
        assert!((ratio - e).abs() < 0.05, "time ratio {ratio} vs {e}");
    }
    assert_eq!(*out.last().unwrap(), 0);
    assert!(matches!(sim.state.mode, EnvelopeMode::Ball(BallState { phase: BallPhase::Rest, .. })));
}

#[test]
fn ball_first_drop_takes_the_drop_time_and_is_parabolic() {
    let mut sim = Sim::new(BALL, [250, 0, 500, 1000]);
    sim.trigger();
    let out = sim.run(stage_samples(250) as usize + 10, false);
    let n = stage_samples(250) as usize;
    // half way through the drop, a parabola has lost a quarter of its height
    let half = out[n / 2] as i32;
    assert!((half - 3071).abs() < 40, "{half}");
    assert!(out[n - 2] < 20 && out[n - 10] > 0);
}

#[test]
fn ball_lift_holds_and_drops() {
    let mut sim = Sim::new(BALL, [100, 800, 500, 600]);
    let lift = sim.run(3000, true);
    let top = lift[2999];
    assert!(top.abs_diff(2457) < 5, "{top}");
    assert!(lift.windows(2).all(|w| w[1] >= w[0]));
    // triggers while held are ignored
    sim.step(true, true);
    assert_eq!(sim.state.last_value, top);
    let fall = sim.run(200, false);
    assert!(fall[199] < top);
}

#[test]
fn adsr_linear_setting_matches_the_old_ramp() {
    // attack 300 with the default (linear) curve: a straight ramp
    let mut sim = Sim::new(ADSR, [300, 300, 1000, 300]);
    let n = stage_samples(300) as usize;
    let out = sim.run(n, true);
    for i in [n / 4, n / 2, 3 * n / 4] {
        let expected = (i as f64 / n as f64 * 4096.0) as i32;
        assert!((out[i] as i32 - expected).abs() <= 8, "{i}: {} vs {expected}", out[i]);
    }
}

#[test]
fn adsr_curves_restart_without_jumps() {
    for (attack, release) in [(0u8, 244u8), (244, 0), (60, 200)] {
        let settings = Settings {
            attack_curve: attack,
            release_curve: release,
            ..Settings::DEFAULT
        };
        let mut sim = Sim::with_settings(ADSR, [300, 300, 600, 300], settings);
        let mut out = sim.run(400, true);
        out.extend(sim.run(300, false));
        out.extend(sim.run(300, true));
        out.extend(sim.run(200, false));
        assert!(max_jump(&out) < 120, "curves {attack} {release}: {}", max_jump(&out));
    }
}

#[test]
fn gate_behaviour_reset_restarts_from_zero() {
    for mode in [ADSR, AR] {
        let settings = Settings {
            gate_behaviour: GateBehaviour::Reset,
            ..Settings::DEFAULT
        };
        let mut sim = Sim::with_settings(mode, [100, 300, 600, 300], settings);
        sim.run(600, true);
        sim.run(100, false);
        assert!(sim.state.last_value > 1000);
        // (AR's default curve knob gives a steep start)
        assert!(sim.step(true, false) < 100);
    }
}

#[test]
fn gate_behaviour_legato_ignores_retriggers_while_open() {
    for mode in [ADSR, AR] {
        let settings = Settings {
            gate_behaviour: GateBehaviour::Legato,
            ..Settings::DEFAULT
        };
        let mut sim = Sim::with_settings(mode, [100, 100, 500, 300], settings);
        sim.run(3000, true);
        let held = sim.state.last_value;
        sim.step(true, true);
        let after = sim.run(100, true);
        assert!(after.iter().all(|&v| v == held), "mode {mode}");
        // but a retrigger during the release still works
        sim.run(100, false);
        let releasing = sim.state.last_value;
        sim.trigger();
        let after = sim.run(20, false);
        assert!(after[19] > releasing, "mode {mode}");
    }
}

#[test]
fn gate_behaviour_cycle_loops_while_the_gate_is_held() {
    for mode in [ADSR, AR] {
        let settings = Settings {
            gate_behaviour: GateBehaviour::Cycle,
            ..Settings::DEFAULT
        };
        let mut sim = Sim::with_settings(mode, [100, 100, 100, 100], settings);
        let out = sim.run(5000, true);
        assert!(peaks(&out).len() >= 4, "mode {mode}: {}", peaks(&out).len());
        assert!(max_jump(&out) < 200);
        // released: no more cycles
        let out = sim.run(5000, false);
        assert_eq!(*out.last().unwrap(), 0);
        assert!(peaks(&out).len() <= 1);
    }
}

#[test]
fn ar_gate_edges_compute_a_new_sample() {
    // the first sample after a gate edge is a step of the new stage, not a repeat
    let mut sim = Sim::new(AR, [100, 500, 100, 500]);
    let before = sim.run(3, false);
    let first = sim.step(true, false);
    assert!(first > before[2]);
    sim.run(2000, true);
    let top = sim.state.last_value;
    let first = sim.step(false, false);
    assert!(first < top);
}
