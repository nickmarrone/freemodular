#[path = "../../../Firmware/src/exponential_curves.rs"]
pub mod exponential_curves;
#[path = "../../../../../fm-lib/src/ringbuffer.rs"]
pub mod ringbuffer;
#[path = "../../../Firmware/src/settings.rs"]
pub mod settings;

#[cfg(test)]
mod settings_tests;

#[cfg(test)]
mod tests {
    use crate::exponential_curves::*;
    use crate::ringbuffer::*;
    use fixed::{types::extra::U16, FixedU16};

    fn frac(bits: u16) -> FixedU16<U16> {
        FixedU16::<U16>::from_bits(bits)
    }

    /// (2^(16xc) - 1) / (2^16c - 1), with c negative bending the other way
    fn ideal(x: f64, c: f64, negative: bool) -> f64 {
        let k = if negative { -16.0 * c } else { 16.0 * c };
        if k.abs() < 1e-9 {
            return x;
        }
        ((k * x).exp2() - 1.0) / (k.exp2() - 1.0)
    }

    /// The original (pre-2026) implementation, for comparison
    fn original_exp_curve(x: FixedU16<U16>, c: FixedU16<U16>, c_negative: bool) -> u16 {
        use fixed::FixedU32;
        let lut = |i: usize| {
            let b = include_bytes!("../../../Firmware/exp2lut.bin");
            FixedU32::<U16>::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap())
        };
        let exp2 = |x: FixedU16<U16>| {
            let lo = x.to_bits() >> 8;
            let hi = u16::min(255, lo + 1);
            let rem = FixedU32::<U16>::from_bits((x.to_bits() << 8) as u32);
            rem.lerp(lut(lo as usize), lut(hi as usize))
        };
        let a = exp2(x * c);
        let b = exp2(c);
        const ONE: FixedU32<U16> = FixedU32::<U16>::from_bits(1u32 << 16);
        let (n, d) = if c_negative { (ONE - a.recip(), ONE - b.recip()) } else { (a - ONE, b - ONE) };
        if d.is_zero() {
            return (FixedU32::<U16>::from(x) * FixedU32::<U16>::from_num(4096)).to_num::<u16>();
        }
        if n == d {
            return 4095;
        }
        ((n / d) * 4096u32).to_num::<u16>()
    }

    #[test]
    fn exp_curve_matches_formula() {
        let mut worst = 0.0f64;
        let mut worst_original = 0.0f64;
        for c_bits in (0..62_592u32).step_by(997) {
            for negative in [false, true] {
                for x_bits in (0..65_536u32).step_by(331) {
                    let (x, c) = (x_bits as f64 / 65536.0, c_bits as f64 / 65536.0);
                    let got = exp_curve(frac(x_bits as u16), frac(c_bits as u16), negative) as f64;
                    let want = ideal(x, c, negative) * 4096.0;
                    let err = (got - want).abs();
                    worst = worst.max(err);
                    let orig = original_exp_curve(frac(x_bits as u16), frac(c_bits as u16), negative) as f64;
                    worst_original = worst_original.max((orig - want).abs());
                    assert!(got <= 4095.0);
                    assert!(err < 12.0, "x={x} c={c} neg={negative}: got {got}, want {want:.1}");
                }
            }
        }
        println!("worst exp_curve error: {worst:.2} LSB (original implementation: {worst_original:.2} LSB)");
        assert!(worst <= worst_original + 1.0);
    }

    #[test]
    fn exp_curve_is_monotonic_and_hits_endpoints() {
        for c_bits in (0..62_592u32).step_by(2_011) {
            for negative in [false, true] {
                let mut last = 0;
                for x_bits in (0..65_536u32).step_by(64) {
                    let v = exp_curve(frac(x_bits as u16), frac(c_bits as u16), negative);
                    assert!(v + 1 >= last, "not monotonic at c={c_bits} x={x_bits} neg={negative}");
                    last = v;
                }
                assert!(exp_curve(frac(0), frac(c_bits as u16), negative) <= 2);
                assert!(exp_curve(frac(u16::MAX), frac(c_bits as u16), negative) >= 4090);
            }
        }
    }

    #[test]
    fn inverse_round_trips() {
        let mut worst = 0;
        // exp_curve_inverse is used to restart a stage from the current output level
        // without a jump, so curve(inverse(v)) must land back near v
        for c_bits in [0u16, 128, 5_000, 20_000, 40_000, 62_000] {
            for negative in [false, true] {
                for v in (0..4096u32).step_by(97) {
                    let level = frac((v << 4) as u16);
                    let t = exp_curve_inverse(level, frac(c_bits), negative);
                    let back = exp_curve(t, frac(c_bits), negative) as i32;
                    worst = worst.max((back - v as i32).abs());
                    assert!((back - v as i32).abs() <= 48,
                        "c={c_bits} neg={negative}: level {v} -> t {} -> {back}", t.to_bits());
                }
            }
        }
        println!("worst inverse round trip error: {worst} LSB");
    }

    fn ring(versions: &[u16]) -> (u16, u16) {
        find_ringbuffer_head(versions.len() as u16, |i| versions[i as usize])
    }

    #[test]
    fn ringbuffer_head() {
        const E: u16 = EMPTY;
        assert_eq!(ring(&[E, E, E, E, E]), (0, E));
        assert_eq!(ring(&[0, E, E, E, E]), (0, 0));
        assert_eq!(ring(&[0, 1, 2, E, E]), (2, 2));
        assert_eq!(ring(&[0, 1, 2, 3, E]), (3, 3));
        // head in the last slot (used to return slot 0)
        assert_eq!(ring(&[0, 1, 2, 3, 4]), (4, 4));
        assert_eq!(ring(&[5, 6, 7, 8, 9]), (4, 9));
        // wrapped around
        assert_eq!(ring(&[5, 1, 2, 3, 4]), (0, 5));
        assert_eq!(ring(&[5, 6, 7, 3, 4]), (2, 7));
        assert_eq!(ring(&[5, 6, 7, 8, 4]), (3, 8));
        // every head position for a larger ring, through several laps
        for len in [2u16, 3, 7, 341] {
            for writes in 1..(3 * len as u32 + 2) {
                let mut slots = vec![E; len as usize];
                for v in 0..writes {
                    slots[(v % len as u32) as usize] = v as u16;
                }
                let head = ((writes - 1) % len as u32) as u16;
                assert_eq!(ring(&slots), (head, writes as u16 - 1), "len {len} writes {writes}");
            }
        }
    }
}
