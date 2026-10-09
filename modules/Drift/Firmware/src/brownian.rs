use fm_lib::rng::ParallelLfsr;

use crate::shared::{exp2_sixteenths, DriftModule};

pub struct BrownianModuleState {
    target_value: u16,
    /// The smoothed output with 16 fractional bits, so that small steps toward
    /// the target add up instead of rounding away
    current_value: u32,
    rng: ParallelLfsr,
}

impl BrownianModuleState {
    pub fn new(random_seed: u16) -> Self {
        let rng = ParallelLfsr::new(random_seed);
        // start in the middle of the range rather than pinned at the bottom
        const START: u16 = 1 << 15;
        Self {
            target_value: START,
            current_value: (START as u32) << 16,
            rng,
        }
    }

    fn step_target_value(&mut self, cv: u16) {
        let step_size = (256 + cv) >> 1;
        let cutoff = cv << 6;

        let random = self.rng.next();
        if random < cutoff {
            const CENTERING_MARGIN: u16 = 5;
            const CENTERING_STRENGTH: u16 = 64;
            let cutoff2 = if self.target_value < u16::MAX / CENTERING_MARGIN {
                (cutoff / 2) - (cutoff / CENTERING_STRENGTH)
            } else if self.target_value > u16::MAX - (u16::MAX / CENTERING_MARGIN) {
                (cutoff / 2) + (cutoff / CENTERING_STRENGTH)
            } else {
                cutoff / 2
            };

            if random >= cutoff2 {
                self.target_value = self.target_value.saturating_add(step_size);
            } else {
                self.target_value = self.target_value.saturating_sub(step_size);
            }
        }
    }

    fn step_smoothed_value(&mut self, cv: u16) {
        let target = (self.target_value as u32) << 16;
        if cv >= 1020 {
            self.current_value = target;
            return;
        }

        // Exponential from 16/65536 of the way per sample (a ~1.6s time constant) at 0
        // to 1/8 (~3ms) at the top, so the knob ends close to the unsmoothed
        // signal instead of jumping to it from ~0.2s
        const MAX_STEP_SIZE: u32 = 1 << 13;
        let step_size = u32::min(exp2_sixteenths(((cv as u32 * 146) >> 10) as u16) >> 11, MAX_STEP_SIZE);

        // move `step_size` of the way to the target (both 16.16 fixed point)
        let delta = self.current_value.abs_diff(target);
        let step = (delta >> 16) * step_size + (((delta & 0xFFFF) * step_size) >> 16);
        if self.current_value < target {
            self.current_value += step;
        } else {
            self.current_value -= step;
        }
    }
}

impl DriftModule for BrownianModuleState {
    fn step(&mut self, cv: &[u16; 4]) -> u16 {
        // TODO: These controls would maybe be more useful if they had an exponential curve (especially texture)
        self.step_target_value(u16::min(1023, cv[2] + cv[0]));
        self.step_smoothed_value(u16::min(1023, cv[3] + cv[1]));
        (self.current_value >> 20) as u16
    }
}
