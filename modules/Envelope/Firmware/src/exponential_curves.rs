use fixed::{types::extra::U16, FixedU16, FixedU32};

const LUT_SIZE: usize = 256;
const U32_BYTES: usize = u32::BITS as usize / 8;
const U16_BYTES: usize = u16::BITS as usize / 8;

#[cfg(target_arch = "avr")]
avr_progmem::progmem! {
    static progmem EXP_LUT: [u8; LUT_SIZE * U32_BYTES] = *include_bytes!("../exp2lut.bin");
    static progmem LOG_LUT: [u8; LUT_SIZE * U16_BYTES] = *include_bytes!("../log2lut.bin");
}

#[cfg(target_arch = "avr")]
fn exp_lut_bytes(i: usize) -> [u8; U32_BYTES] {
    EXP_LUT.load_sub_array::<U32_BYTES>(U32_BYTES * i)
}

#[cfg(target_arch = "avr")]
fn log_lut_bytes(i: usize) -> [u8; U16_BYTES] {
    LOG_LUT.load_sub_array::<U16_BYTES>(U16_BYTES * i)
}

// On the host (unit tests) the tables are plain arrays
#[cfg(not(target_arch = "avr"))]
static EXP_LUT: [u8; LUT_SIZE * U32_BYTES] = *include_bytes!("../exp2lut.bin");
#[cfg(not(target_arch = "avr"))]
static LOG_LUT: [u8; LUT_SIZE * U16_BYTES] = *include_bytes!("../log2lut.bin");

#[cfg(not(target_arch = "avr"))]
fn exp_lut_bytes(i: usize) -> [u8; U32_BYTES] {
    EXP_LUT[U32_BYTES * i..U32_BYTES * (i + 1)].try_into().unwrap()
}

#[cfg(not(target_arch = "avr"))]
fn log_lut_bytes(i: usize) -> [u8; U16_BYTES] {
    LOG_LUT[U16_BYTES * i..U16_BYTES * (i + 1)].try_into().unwrap()
}

fn lut_load_fixed32(i: usize) -> FixedU32<U16> {
    debug_assert!(i < LUT_SIZE);
    FixedU32::<U16>::from_le_bytes(exp_lut_bytes(i))
}

fn lut_load_u16(i: usize) -> u16 {
    debug_assert!(i < LUT_SIZE);
    u16::from_le_bytes(log_lut_bytes(i))
}

/**
Returns 2^16x by finding the two nearest entries in the lookup table and
interpolating between them.

The interpolation is done in plain u32 math: a fixed-point `lerp` would need a
64-bit multiply, which is very slow on AVR. The step between entries is split into
high and low bytes so the products fit in 32 bits.
*/
fn exp2_lut(x: FixedU16<U16>) -> FixedU32<U16> {
    let idx_low = (x.to_bits() >> 8) as usize;
    let idx_high = usize::min(LUT_SIZE - 1, idx_low + 1);
    let remainder = (x.to_bits() & 0xff) as u32;
    let v_low = lut_load_fixed32(idx_low).to_bits();
    let v_high = lut_load_fixed32(idx_high).to_bits();
    let step = v_high - v_low;
    FixedU32::<U16>::from_bits(v_low + (step >> 8) * remainder + (((step & 0xff) * remainder) >> 8))
}

/**
Computes the equation (2^(16xc) - 1) / (2^16c - 1)
- x and c are both positive fractions (0 <= x < 1)
- c_negative indicates whether c should be interpreted as a negative number,
which will cause the curve to bend the other direction
- returns a number between 0 and 4095 (0xFFF) inclusive

This runs for every sample in the curved modes, so it avoids 64-bit math (which
the fixed-point division and reciprocal need, and which is very slow on AVR):
- the negative curve is the positive one rotated 180 degrees, f(-c, x) = 1 - f(c, 1 - x),
  so no reciprocals are needed
- the ratio is computed with one 32-bit division after scaling both terms down
*/
#[inline(never)]
pub fn exp_curve(x: FixedU16<U16>, c: FixedU16<U16>, c_negative: bool) -> u16 {
    if c_negative {
        let flipped = FixedU16::<U16>::from_bits(u16::MAX - x.to_bits());
        MAX_OUTPUT - exp_curve_positive(flipped, c)
    } else {
        exp_curve_positive(x, c)
    }
}

const MAX_OUTPUT: u16 = 4095;

fn exp_curve_positive(x: FixedU16<U16>, c: FixedU16<U16>) -> u16 {
    const ONE: u32 = 1 << 16;
    let a = exp2_lut(x * c).to_bits();
    let b = exp2_lut(c).to_bits();
    debug_assert!(a >= ONE && b >= ONE && a <= b);
    let mut numerator = a - ONE;
    let mut denominator = b - ONE;
    if denominator == 0 {
        // c == 0: linear
        return ((x.to_bits() as u32 * (MAX_OUTPUT as u32 + 1)) >> 16) as u16;
    }
    // keep numerator * 4096 within u32
    while denominator >= 1 << 20 {
        numerator >>= 1;
        denominator >>= 1;
    }
    u32::min(numerator * (MAX_OUTPUT as u32 + 1) / denominator, MAX_OUTPUT as u32) as u16
}

/**
Efficiently computes the ABSOLUTE VALUE of the base 2 log of any value between
0 and 2^16, exclusive.

First computes the largest power of 2 strictly less than log2(x) by counting
leading zeros in the binary representation of x (which gives the integer part of
the result); then looks up the fractional part in a table of values of log2(n) for
n in [0.5, 1] and adds the two parts together.
*/
pub fn fixed_point_log2(mut x: FixedU32<U16>) -> FixedU32<U16> {
    debug_assert!(x != 0);

    if x < 1 {
        // It seems like there is a slight loss of precision here (and an extra computation).
        // It might be better to use a separate lookup table and interpolate here, in
        // the same way as computing 2^x (or re-purpose the existing [0.5, 1] table).
        // But I don't want to deal with that.
        // For now, I'm taking advantage of the identity that log2(x) = -log2(1/x)
        // to ensure that x is always >= 1, so the output of the function is always
        // positive
        x = x.recip();
        debug_assert!(x >= 1);
    }

    // Count the number of leading zeros to calculate floor(log2(x))
    let lz = x.leading_zeros() as u32;
    let integer_part = 15 - lz;

    // Shift x to the left to normalize it (i.e. make the MSB 1)
    let normalized = x.to_bits() << lz;

    // The 8 bits after the leading 1 index the table of log2(1 + i/256); the next
    // 8 bits interpolate between entries. Without interpolation, values just
    // above 1 (gentle curves) all round down to log2 = 0.
    let fractional_part_index = ((normalized >> 23) & 0xFF) as usize;
    let remainder = (normalized >> 15) & 0xFF;
    let low = lut_load_u16(fractional_part_index) as u32;
    let high = if fractional_part_index == 255 {
        1 << 16 // log2(2)
    } else {
        lut_load_u16(fractional_part_index + 1) as u32
    };
    let fractional_part = low + (((high - low) * remainder) >> 8);

    // Combine the integer part and the fractional part
    FixedU32::<U16>::from_bits((integer_part << 16) + fractional_part)
}

/**
Calculates the inverse of the exp_curve function s.t. exp_curve_inverse(exp_curve(x, c) / 4096, c) ~= x

The formula is log2(x * (2^c - 1) + 1) / c
*/
pub fn exp_curve_inverse(x: FixedU16<U16>, c: FixedU16<U16>, c_negative: bool) -> FixedU16<U16> {
    if c_negative {
        // Same symmetry as exp_curve: f(-c)^-1(y) = 1 - f(c)^-1(1 - y). This avoids
        // reciprocals and logs of values below 1, which lose a lot of precision.
        let flipped = FixedU16::<U16>::from_bits(u16::MAX - x.to_bits());
        let t = exp_curve_inverse_positive(flipped, c);
        FixedU16::<U16>::from_bits(u16::MAX - t.to_bits())
    } else {
        exp_curve_inverse_positive(x, c)
    }
}

fn exp_curve_inverse_positive(x: FixedU16<U16>, c: FixedU16<U16>) -> FixedU16<U16> {
    if c == 0 {
        // linear
        return x;
    }

    const ONE: FixedU32<U16> = FixedU32::<U16>::from_bits(1u32 << 16);

    let coefficient = exp2_lut(c) - ONE;
    let a = (Into::<FixedU32<U16>>::into(x) * coefficient) + ONE;

    let numerator = fixed_point_log2(a);
    let denominator = FixedU32::<U16>::from(c) * 16;
    debug_assert!(denominator != 0);

    // Table rounding can push the ratio slightly past 1; clamp instead of wrapping
    // around to 0 (which would restart the stage from the bottom)
    FixedU16::<U16>::from_bits(u32::min((numerator / denominator).to_bits(), u16::MAX as u32) as u16)
}
