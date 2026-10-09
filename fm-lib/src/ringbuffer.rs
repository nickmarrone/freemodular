//! Hardware-independent helpers for the wear-levelled EEPROM ring buffer, kept
//! separate so they can be unit tested on the host.

/// Marker for an empty (never written / erased) slot
pub const EMPTY: u16 = 0xFFFF;

/**
Finds the most recently written slot of a ring buffer of `len` slots whose version
numbers increase by one with each write, wrapping back to slot 0 after the last
one. `load(i)` returns the version stored in slot `i`, or `EMPTY`.

Returns `(index, version)` of the newest slot, or `(0, EMPTY)` if all are empty.
Uses a binary search for the point where versions stop increasing, so it only
reads O(log n) slots.
*/
pub fn find_ringbuffer_head(len: u16, load: impl Fn(u16) -> u16) -> (u16, u16) {
    let midpoint = |low: u16, high: u16| {
        debug_assert!(low <= high);
        low + (high - low) / 2
    };

    // empty slots sort before everything
    let gt = |a: u16, b: u16| {
        if b == EMPTY && a != EMPTY {
            true
        } else if a == EMPTY {
            false
        } else {
            a > b
        }
    };

    let mut low_idx = 0;
    let mut high_idx = len - 1;
    let mut low_value = load(low_idx);
    let high_value = load(high_idx);

    // No drop anywhere: either everything is empty or versions increase all the
    // way to the last slot, which is then the newest
    if !gt(low_value, high_value) {
        return if high_value == EMPTY {
            (0, EMPTY)
        } else {
            (high_idx, high_value)
        };
    }

    // Invariant: load(low) > load(high), so the head is in [low, high)
    while low_idx + 1 < high_idx {
        let mid_idx = midpoint(low_idx, high_idx);
        let mid_value = load(mid_idx);
        if gt(low_value, mid_value) {
            high_idx = mid_idx;
        } else {
            low_idx = mid_idx;
            low_value = mid_value;
        }
    }
    (low_idx, low_value)
}
