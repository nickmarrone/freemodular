#![allow(dead_code, static_mut_refs)]
#[path = "../../../Firmware/src/clock.rs"]
pub mod clock;
#[path = "../../../Firmware/src/menu/utils.rs"]
pub mod utils;

#[cfg(test)]
mod tests {
    use crate::clock::*;
    use crate::utils::*;

    fn cfg(bpm10: u16) -> ClockConfig {
        let mut c = ClockConfig::new();
        c.bpm10 = bpm10;
        c
    }

    /// run n ticks; return rising-edge tick indices per channel, and high lengths
    fn run(n: u32, start_tick: u32, prev: &mut u8, edges: &mut [Vec<u32>; 8], highs: &mut [Vec<u32>; 8], rise: &mut [u32; 8]) {
        for t in start_tick..start_tick + n {
            let out = test_tick();
            for ch in 0..8 {
                let now = out >> ch & 1 == 1;
                let was = *prev >> ch & 1 == 1;
                if now && !was { edges[ch].push(t); rise[ch] = t; }
                if !now && was { highs[ch].push(t - rise[ch]); }
            }
            *prev = out;
        }
    }

    struct Sim { t: u32, prev: u8, edges: [Vec<u32>; 8], highs: [Vec<u32>; 8], rise: [u32; 8] }
    impl Sim {
        fn new(c: &ClockConfig) -> Self { start(c); Sim { t: 0, prev: 0, edges: Default::default(), highs: Default::default(), rise: [0; 8] } }
        fn run(&mut self, n: u32) { run(n, self.t, &mut self.prev, &mut self.edges, &mut self.highs, &mut self.rise); self.t += n; }
    }

    #[test]
    fn all() {
        default_divisions();
        tuplets_lock_to_master();
        swing_and_phase();
        euclid_tresillo();
        realign_mid_run();
        tempo_change_keeps_lock();
        quantized_stop_and_special_outputs();
        pulse_width_limits();
        probability();
        division_stepping();
        slow_division_high_pw_no_overflow();
    }

    fn default_divisions() {
        let c = cfg(1200); // 120bpm -> 4000 ticks per beat
        let mut s = Sim::new(&c);
        s.run(8000 * 20);
        let expect_period = [4000, 2000, 1000, 500, 8000, 16000, 32000, 64000];
        for ch in 0..8 {
            let e = &s.edges[ch];
            assert!(!e.is_empty(), "ch{ch} no edges");
            for (k, t) in e.iter().enumerate() {
                assert_eq!(*t, k as u32 * expect_period[ch], "ch{ch} edge {k}");
            }
            // 50% pulse width
            for h in &s.highs[ch] { assert_eq!(*h, expect_period[ch] / 2, "ch{ch} pw"); }
        }
    }

    fn tuplets_lock_to_master() {
        let mut c = cfg(1200);
        c.channels[0].tuplet = TUPLET_TRIPLET; // x1.5 -> 3 per 2 beats
        c.channels[1].tuplet = TUPLET_DOTTED;  // x2 * 2/3 -> 4 per 3 beats
        let mut s = Sim::new(&c);
        s.run(8000 * 12);
        // triplet: every 3rd edge lands exactly on a 2-beat boundary
        for (k, t) in s.edges[0].iter().enumerate() {
            let exact = k as f64 * 8000.0 / 3.0;
            assert!((*t as f64 - exact).abs() <= 1.0, "triplet edge {k} at {t}, exact {exact}");
            if k % 3 == 0 { assert_eq!(*t, k as u32 / 3 * 8000); }
        }
        for (k, t) in s.edges[1].iter().enumerate() {
            let exact = k as f64 * 3000.0;
            assert!((*t as f64 - exact).abs() <= 1.0, "dotted edge {k} at {t}");
        }
    }

    fn swing_and_phase() {
        let mut c = cfg(1200);
        c.channels[1].swing = 16; // x2 period 2000, odd periods delayed 500
        c.channels[0].phase_shift = 16; // x1 delayed by 1000
        c.channels[2].phase_shift = -16; // x4 period 1000, advanced 250
        let mut s = Sim::new(&c);
        s.run(8000 * 4);
        for (k, t) in s.edges[1].iter().enumerate() {
            let exp = k as u32 * 2000 + if k % 2 == 1 { 500 } else { 0 };
            assert_eq!(*t, exp, "swing edge {k}");
        }
        for (k, t) in s.edges[0].iter().enumerate() {
            assert_eq!(*t, k as u32 * 4000 + 1000, "delay edge {k}");
        }
        // advanced channel: starts mid-pulse (1/4 into period), then edges at 750, 1750...
        assert_eq!(s.edges[2][0], 0);
        assert_eq!(s.edges[2][1], 750);
        assert_eq!(s.edges[2][2], 1750);
    }

    fn euclid_tresillo() {
        let mut c = cfg(1200);
        c.channels[2].euclid_steps = 8; // x4: 1000 ticks per step
        c.channels[2].euclid_fill = 3;
        c.channels[3].euclid_steps = 8; // x8 rotated by 1
        c.channels[3].euclid_fill = 3;
        c.channels[3].euclid_rotate = 1;
        c.channels[3].division = 4;
        let mut s = Sim::new(&c);
        s.run(8000 * 8);
        let steps: Vec<u32> = s.edges[2].iter().map(|t| t / 1000).collect();
        assert_eq!(&steps[..6], &[0, 3, 6, 8, 11, 14]);
        let rot: Vec<u32> = s.edges[3].iter().map(|t| t / 1000).collect();
        // rotation 1: pattern starting at step 1 of E(3,8): x..x..x. -> hits at 2, 5, 7
        assert_eq!(&rot[..6], &[2, 5, 7, 10, 13, 15]);
    }

    fn realign_mid_run() {
        let mut c = cfg(1200);
        let mut s = Sim::new(&c);
        s.run(12345);
        c.channels[3].division = 4; // x8 -> x4
        c.channels[4].division = -3; // /2 -> /3
        apply_config(&c, 0b11000);
        let before3 = s.edges[3].len();
        let before4 = s.edges[4].len();
        s.run(8000 * 10);
        for t in &s.edges[3][before3..] { assert_eq!(t % 1000, 0, "x4 realigned edge {t}"); }
        for t in &s.edges[4][before4..] { assert_eq!(t % 12000, 0, "/3 realigned edge {t}"); }
    }

    fn tempo_change_keeps_lock() {
        let mut c = cfg(1200);
        let mut s = Sim::new(&c);
        s.run(10001);
        c.bpm10 = 900;
        apply_config(&c, 0);
        s.run(8000 * 10);
        c.bpm10 = 1373;
        apply_config(&c, 0);
        s.run(8000 * 10);
        // every x1 edge coincides with every 2nd x2 edge, every 4th x4 edge, and /2 every 2nd x1
        for e in &s.edges[0] {
            assert!(s.edges[1].contains(e), "x2 missing edge at {e}");
            assert!(s.edges[2].contains(e), "x4 missing edge at {e}");
        }
        for e in &s.edges[4] { assert!(s.edges[0].contains(e), "/2 not on beat {e}"); }
        // no runt or doubled edges: spacing of x1 edges consistent with tempos
        for w in s.edges[0].windows(2) {
            let d = w[1] - w[0];
            assert!(d >= 3490 && d <= 5340, "x1 spacing {d}");
        }
    }

    fn quantized_stop_and_special_outputs() {
        let mut c = cfg(1200);
        c.channels[5].division = DIV_STOP;
        c.channels[6].division = DIV_RUN;
        c.channels[7].division = DIV_RESET;
        let mut s = Sim::new(&c);
        s.run(5000);
        assert_eq!(s.edges[7], vec![0]);
        assert_eq!(s.highs[7], vec![40]); // 5ms reset trigger
        assert_eq!(s.edges[6], vec![0]);
        assert!(s.edges[5].is_empty());
        stop(STOP_BAR);
        assert!(stop_is_pending());
        s.run(12000); // to tick 17000; bar ends at 16000
        assert!(!is_running());
        assert_eq!(s.edges[5], vec![16000 - 1 + 1]); // STOP goes high at the bar line
        assert!(s.edges[0].iter().all(|t| *t < 16000), "no new beat after stop");
        assert_eq!(*s.highs[6].last().unwrap(), 16000); // RUN gate length
        // restart
        start(&c);
        s.run(100);
        assert_eq!(*s.edges[0].last().unwrap(), 17000);
        // immediate stop mid-pulse keeps pulse >= 5ms
        s.run(1990); // x8 (ch3) pulse at 17000+500k; at tick 19090 ch3 just went high at 19000? check hold
        let rises = s.edges[3].len();
        stop(STOP_NOW);
        s.run(200);
        assert_eq!(s.edges[3].len(), rises);
        let h = *s.highs[3].last().unwrap();
        assert!(h >= 40, "pulse cut to {h}");
    }

    fn pulse_width_limits() {
        let mut c = cfg(1200);
        c.channels[0].pulse_width = 0; // trig: 40 ticks
        c.channels[1].pulse_width = 100; // inverted trig: low for 40 ticks
        c.channels[2].pulse_width = 1; // x4 period 400, 1% = 4 -> clamped to 40
        c.channels[3].division = 64; // x64 @ 300bpm = 25 ticks period: 50%
        c.bpm10 = 3000;
        let mut s = Sim::new(&c);
        s.run(8000 * 2);
        assert!(s.highs[0].iter().all(|h| *h == 40), "{:?}", &s.highs[0][..3]);
        assert!(s.highs[1].iter().all(|h| *h == 800 - 40));
        assert!(s.highs[2].iter().all(|h| *h == 40));
        assert!(s.highs[3].iter().all(|h| *h == 12 || *h == 13), "{:?}", &s.highs[3][..5]);
    }

    fn probability() {
        let mut c = cfg(1200);
        c.channels[3].probability = 50; // x8, 2 per... 500 ticks
        c.channels[2].probability = 0;
        let mut s = Sim::new(&c);
        s.run(8000 * 60);
        let n = s.edges[3].len();
        assert!(n > 300 && n < 660, "50% prob fired {n} of 960");
        assert!(s.edges[2].len() <= 1);
    }

    fn slow_division_high_pw_no_overflow() {
        let mut c = cfg(200); // 20bpm: beat = 24000 ticks
        c.channels[7].division = -64;
        c.channels[7].pulse_width = 99;
        let p = 24000u32 * 64;
        let mut s = Sim::new(&c);
        s.run(p + 10);
        assert_eq!(s.highs[7], vec![p * 99 / 100]);
    }

    fn division_stepping() {
        assert_eq!(step_clock_division(1, -1), -2);
        assert_eq!(step_clock_division(-2, 1), 1);
        assert_eq!(step_clock_division(-32, -1), -64);
        assert_eq!(step_clock_division(-64, -1), DIV_STOP);
        assert_eq!(step_clock_division(DIV_STOP, -1), DIV_RUN);
        assert_eq!(step_clock_division(DIV_RESET, -1), DIV_RESET);
        assert_eq!(step_clock_division(DIV_STOP, 1), -64);
        assert_eq!(step_clock_division(-64, 1), -32);
        assert_eq!(step_clock_division(64, 1), 64);
        assert_eq!(step_clock_division(3, 1), 4);
        assert_eq!(single_step_clock_division(-2, 1), 1);
        assert_eq!(single_step_clock_division(DIV_RUN, 2), -64);
        assert_eq!(single_step_clock_division(DIV_RESET, -1), DIV_RESET);
        assert!(ClockConfig::new().is_valid());
        let mut c = ClockConfig::new();
        c.channels[0].division = 0;
        assert!(!c.is_valid());
    }
}
