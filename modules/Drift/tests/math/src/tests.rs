use crate::bezier::BezierModuleState;
use crate::brownian::BrownianModuleState;
use crate::lfo::LfoModuleState;
use crate::perlin::{perlin_segment, PerlinModuleState};
use crate::shared::{exp2_sixteenths, get_delta_t, DriftModule, MICROS_PER_SAMPLE};
use fixed::types::{I1F15, U0F16};

/// Small deterministic generator for test inputs
struct TestRng(u32);
impl TestRng {
    fn next(&mut self, n: u32) -> u32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 8) % n
    }
}

fn ideal_delta_t(octaves: f64) -> f64 {
    let hz = 2f64.powf(octaves) / 40.0;
    hz * MICROS_PER_SAMPLE as f64 / 1e6 * 2f64.powi(32)
}

#[test]
fn exp2_sixteenths_matches_formula() {
    for s in 0..16 * 13 {
        let ideal = 32768.0 * 2f64.powf(s as f64 / 16.0);
        let got = exp2_sixteenths(s) as f64;
        assert!((got / ideal - 1.0).abs() < 1e-4, "s={s} got={got} ideal={ideal}");
    }
}

#[test]
fn delta_t_tracks_volts_per_octave() {
    let mut last = 0;
    for cv in 0..=1023u16 {
        let dt = get_delta_t(0, cv, 0);
        let octaves = cv as f64 * 20.0 / 4096.0;
        let ideal = ideal_delta_t(octaves);
        // linear interpolation between sixteenths of an octave is within ~0.03%
        assert!((dt as f64 / ideal - 1.0).abs() < 5e-4, "cv={cv} dt={dt} ideal={ideal}");
        assert!(dt >= last);
        last = dt;
    }
    // 1/40 Hz at the bottom, ~100Hz (about 24 samples per cycle) at the top
    let top = get_delta_t(1023, 1023, i16::MAX);
    assert_eq!(top, get_delta_t(1023, 0, 0));
    assert!((u32::MAX / top) == 24, "{}", u32::MAX / top);
    assert!((u32::MAX / get_delta_t(0, 0, 0)).abs_diff(100_000) <= 1);
    assert_eq!(get_delta_t(0, 0, i16::MIN), get_delta_t(0, 0, 0));
}

#[test]
fn perlin_segment_stays_in_range() {
    // the module mixes base * (4 - blend) + octave * blend, which only fits I1F15 if
    // every segment value is within +-0.25
    let grads: Vec<I1F15> = (1..=8)
        .flat_map(|g| [I1F15::from_bits(g << 11), -I1F15::from_bits(g << 11)])
        .collect();
    for &a in &grads {
        for &b in &grads {
            let mut last = perlin_segment(U0F16::from_bits(0), a, b);
            assert_eq!(last, 0);
            for x in 1..=u16::MAX {
                let v = perlin_segment(U0F16::from_bits(x), a, b);
                assert!(v.to_bits().abs() <= 1 << 13, "a={a} b={b} x={x} v={v}");
                assert!((v.to_bits() - last.to_bits()).abs() <= 16, "a={a} b={b} x={x}");
                last = v;
            }
            // and it ends at 0, where the next segment starts
            assert!(last.to_bits().abs() <= 4, "a={a} b={b} end={last}");
        }
    }
}

/// Runs a module with the given controls, checking every output is in range, and
/// returns the outputs
fn run(module: &mut dyn DriftModule, cv: [u16; 4], samples: usize) -> Vec<u16> {
    (0..samples)
        .map(|_| {
            let v = module.step(&cv);
            assert!(v <= 4095);
            v
        })
        .collect()
}

fn max_jump(values: &[u16]) -> u16 {
    values.windows(2).map(|w| w[0].abs_diff(w[1])).max().unwrap_or(0)
}

#[test]
fn perlin_is_smooth_at_every_texture() {
    for seed in [1, 0x1234, 0xBEEF] {
        let mut perlin = PerlinModuleState::new(seed);
        for texture in [0, 300, 700, 1023] {
            // 2 segments per second at 12 o'clock; the octave layer runs 4x faster
            let out = run(&mut perlin, [0, 0, 500, texture], 20_000);
            assert!(max_jump(&out) <= 16, "texture {texture}: {}", max_jump(&out));
        }
        // full speed and texture with CV pushed past the end is clamped, not wrapped
        run(&mut perlin, [1023, 1023, 1023, 1023], 20_000);
    }
}

#[test]
fn bezier_stays_in_range_with_any_controls() {
    let mut rng = TestRng(7);
    let mut bezier = BezierModuleState::new(0xACE1);
    for _ in 0..200 {
        let cv = [0, 0, 0, 0].map(|_: u16| rng.next(1024) as u16);
        run(&mut bezier, cv, 500);
    }
}

#[test]
fn bezier_is_continuous_across_segments() {
    let mut bezier = BezierModuleState::new(0xACE1);
    // smooth curves at a fixed rate: 4 segments per second
    let out = run(&mut bezier, [0, 0, 588, 700], 50_000);
    assert!(max_jump(&out) <= 16, "{}", max_jump(&out));
}

#[test]
fn brownian_smoothing_reaches_the_target() {
    let mut brownian = BrownianModuleState::new(0x5EED);
    // wander fast with heavy smoothing, so the output lags the target
    run(&mut brownian, [0, 0, 1023, 0], 5_000);
    // stop wandering and let the smoothing settle for 30 seconds
    let settled = *run(&mut brownian, [0, 0, 0, 0], 75_000).last().unwrap();
    // with texture at max the output is the target itself
    let target = run(&mut brownian, [0, 0, 0, 1023], 1)[0];
    assert!(settled.abs_diff(target) <= 1, "settled {settled} target {target}");
}

#[test]
fn brownian_texture_has_no_jump_at_the_top() {
    // right below the top of the smoothing range the output lags the unsmoothed
    // walk by a few ms (~8 samples), rather than ~0.2s
    let mut a = BrownianModuleState::new(3);
    let mut b = BrownianModuleState::new(3);
    let smoothed = run(&mut a, [0, 0, 1023, 1019], 10_000);
    let raw = run(&mut b, [0, 0, 1023, 1023], 10_000);
    let worst = smoothed.iter().zip(&raw).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
    assert!(worst < 400, "{worst}");
}

#[test]
fn lfo_cycle_length_matches_frequency() {
    for (knob, texture) in [(512, 512), (900, 512), (1023, 512), (700, 100), (700, 900)] {
        let mut lfo = LfoModuleState::new();
        let out = run(&mut lfo, [0, 0, knob, texture], 200_000);
        let dt = get_delta_t(knob, 0, 0) as f64;
        let ideal = 2f64.powi(32) / dt;
        let peaks: Vec<usize> = (1..out.len() - 1)
            .filter(|&i| out[i] == 4095 || (out[i] > out[i - 1] && out[i] >= out[i + 1] && out[i] > 4000))
            .collect();
        let first = peaks[0];
        let last = *peaks.last().unwrap();
        let cycles = ((last - first) as f64 / ideal).round();
        assert!(cycles >= 2.0);
        let measured = (last - first) as f64 / cycles;
        assert!((measured / ideal - 1.0).abs() < 0.02, "knob {knob}: {measured} vs {ideal}");
    }
}

#[test]
fn lfo_texture_changes_are_continuous() {
    let mut rng = TestRng(99);
    let mut lfo = LfoModuleState::new();
    let mut texture: i32 = 512;
    let mut last = lfo.step(&[0, 0, 600, texture as u16]);
    for _ in 0..200_000 {
        // sweep texture around at up to a full turn per 0.4s, keeping it away from
        // the ends where the cycle has a saw edge
        texture = (texture + rng.next(3) as i32 - 1).clamp(60, 960);
        let v = lfo.step(&[0, 0, 600, texture as u16]);
        // the cycle is ~690 samples, so the steepest a 5% texture segment rises is
        // ~120 per sample; moving the peak right in front of the output can make one
        // rise a bit steeper, but never a jump across the range
        assert!(v.abs_diff(last) <= 205, "{last} -> {v} (texture {texture})");
        last = v;
    }
}
