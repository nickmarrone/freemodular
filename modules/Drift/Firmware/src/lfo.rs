use crate::shared::{get_delta_t, DriftModule};

/**
A triangle LFO whose peak position (skew) is set by texture: falling saw at 0,
triangle in the middle, rising saw at full.

The output follows a straight line from (`start_time`, `start_value`) to
(`end_time`, `end_value`) in the 32-bit cycle phase. A cycle rises from 0 to the
peak and falls back to 0 by the end of the phase. When texture moves, the current
line is re-aimed at the new peak from wherever the output is now, so the shape
changes immediately without jumps and without changing the cycle length.
*/
pub struct LfoModuleState {
    time: u32,
    texture: u16,
    value: u16,
    start_time: u32,
    start_value: u16,
    end_time: u32,
    end_value: u16,
}

/// Texture readings closer than this to the one in use are treated as ADC noise
const TEXTURE_DEAD_BAND: u16 = 2;

fn apex_time(texture: u16) -> u32 {
    (texture as u32) << 22
}

impl LfoModuleState {
    pub fn new() -> Self {
        let mut lfo = Self {
            time: 0,
            texture: 512,
            value: 0,
            start_time: 0,
            start_value: 0,
            end_time: 0,
            end_value: 0,
        };
        lfo.start_cycle();
        lfo
    }

    fn set_line(&mut self, start_time: u32, start_value: u16, end_time: u32, end_value: u16) {
        self.start_time = start_time;
        self.start_value = start_value;
        self.end_time = end_time;
        self.end_value = end_value;
    }

    fn start_cycle(&mut self) {
        let apex = apex_time(self.texture);
        if apex == 0 {
            self.set_line(0, u16::MAX, u32::MAX, 0);
        } else {
            self.set_line(0, 0, apex, u16::MAX);
        }
    }

    fn rising(&self) -> bool {
        self.end_value == u16::MAX
    }

    /// Aim the output at the peak for the current texture, starting from the
    /// last output at `last_time`. If the peak is no longer ahead of `time`, fall
    /// from where the output is instead of jumping up to the peak.
    fn reshape(&mut self, last_time: u32, time: u32) {
        let apex = apex_time(self.texture);
        if self.rising() && apex > time {
            self.set_line(last_time, self.value, apex, u16::MAX);
        } else {
            self.set_line(last_time, self.value, u32::MAX, 0);
        }
    }

    fn value_at(&self, time: u32) -> u16 {
        debug_assert!(time >= self.start_time && time <= self.end_time);
        let span = (self.end_time - self.start_time) >> 16;
        if span == 0 {
            return self.end_value;
        }
        let elapsed = (time - self.start_time) >> 16;
        // both are 16 bit, so this is a cheap 32-bit division
        let fraction = u32::min((elapsed << 16) / span, u16::MAX as u32) as i32;
        let delta = self.end_value as i32 - self.start_value as i32;
        (self.start_value as i32 + ((delta * (fraction >> 1)) >> 15)) as u16
    }
}

impl DriftModule for LfoModuleState {
    fn step(&mut self, cv: &[u16; 4]) -> u16 {
        let dt = get_delta_t(cv[2], cv[0], 0);
        let texture = u16::min(1023, cv[3] + cv[1]);
        let last_time = self.time;
        let (time, rollover) = self.time.overflowing_add(dt);
        self.time = time;

        if rollover {
            self.texture = texture;
            self.start_cycle();
        } else if texture.abs_diff(self.texture) >= TEXTURE_DEAD_BAND {
            self.texture = texture;
            self.reshape(last_time, time);
        }
        if self.rising() && time >= self.end_time {
            self.set_line(self.end_time, u16::MAX, u32::MAX, 0);
        }

        self.value = self.value_at(time);
        self.value >> 4
    }
}
