# FM Envelope — Architecture & Review Notes

Developer-facing notes for the Envelope firmware: how it works, what the Oct 2026
review found and fixed, and how to test it. Results come from host unit tests and
simavr runs of the real firmware (see [Testing](#testing)). They have **not** been
verified on a physical module yet.

## Hardware

| Item | Detail |
|---|---|
| MCU | Arduino Nano (ATmega328P, 16 MHz) |
| Output | MCP4922 12-bit DAC, channel A (channel B is shut down and unconnected), SPI with CS on D10 |
| Knobs + CV | 4 channels summed and inverted in hardware, read on A4, A5, ADC6, ADC7 |
| Gate in | D2 (inverted, polled every sample) |
| Trigger in | D3 / INT1 (inverted, falling-edge interrupt) |
| Button | D8 |
| LEDs | D4–D7 (mode / current stage) |
| Aux gate out | A3. Its behaviour is chosen at boot by jumpers on A1/A2: end-of-rise, end-of-fall, non-zero, or follow-gate |
| Spare | D9, D12, A0 (net only), DAC channel B (pin not routed) |

## Firmware architecture

- **Sample clock**: TIMER2 CTC at exactly **2083.3 Hz (480 µs)**. The ISR only raises
  the DAC chip-select, which latches the sample the main loop already shifted in. This
  gives jitter-free output with one sample of latency.
- **Main loop**: as soon as the previous sample has been latched, it reads the gate and
  the queued trigger, calls `envelope::update`, and shifts the result into the DAC with
  CS held low. It also handles the button, the LEDs, and the aux output on stage changes.
- **ADC**: free-running, interrupt driven, cycling through 4 channels (fm-lib
  `async_adc`).
- **Time**: each stage is a u32 phase that advances by `u32::MAX / steps` per sample.
  `steps` comes from the knob through a piecewise exponential response: up to 10 s per
  stage, or 100 s in the long range.
- **Modes** (`envelope/`):
  - `adsr.rs` — ADSR.
  - `acrc.rs` — AR with variable curve (ACRC), plus its looping version.
  - `ahrd.rs` — looping attack-hold-release-delay.
- **Curves** (`exponential_curves.rs`): `(2^(16xc) − 1) / (2^(16c) − 1)` from a
  256-entry `2^x` table. The inverse, used to restart a stage from the current level
  without a jump, uses a `log2` table.
- **Persistence**: one byte through fm-lib's `WearLevelledEepromWriter`. Bits 0–1 are the
  mode and bit 7 is the long time range, so bytes saved by older firmware still load.

## Bugs found and fixed

| Bug | Impact | Fix |
|---|---|---|
| TIMER2 `OCR2A = 120` gives 484 µs per sample, but the time math assumes 480 µs (the comment said 2.27 kHz) | Every stage ~0.8% slower than designed | `OCR2A = 119` |
| ADSR ignored Trig unless the gate was held (Decay/Sustain only) | A Trig with no gate did nothing, contradicting the manual ("Trig pings the envelope") | A Trig with the gate low runs attack → decay → release |
| `exp_curve_inverse` with c = 0 (curve knob exactly centred) returned x/16 | Retriggering restarted the stage at 1/16 of the current level, causing a jump | Returns x |
| `exp_curve_inverse` ratio could exceed 1.0 and wrap around in `as u16` | Retriggering near the top of gentle curves restarted from 0 | Clamped |
| `fixed_point_log2` used only an 8-bit table index | The inverse of gentle curves collapsed to 0, causing a jump on retrigger | Interpolates between table entries |
| Negative-curve inverse went through reciprocals of values < 1 | Up to ~60 LSB error on retrigger | Uses the symmetry f(−c)⁻¹(y) = 1 − f(c)⁻¹(1 − y) |
| fm-lib `find ringbuffer head` returned the slot *index* as the version in one branch | Wrong, non-monotonic versions written; a stale slot could later be loaded | Returns the version |
| fm-lib ring-buffer search returned slot 0 when the newest slot was the last one | Saved mode reverts to a stale value once every 341 boots, and the ring order breaks | Rewritten as a tested pure function (`fm-lib/src/ringbuffer.rs`) |
| fm-lib version overflow reset the address before reading the current data | After ~65k boots: copies the wrong slot with an "empty" version, losing settings | Reads first, then restarts the ring at version 0 |
| fm-lib EEPROM writes (via avr-hal) didn't mask interrupts between EEMPE and EEPE | With the ~9.6 kHz ADC interrupt, a write can occasionally fail silently | Writes run in a critical section, after waiting for the previous write outside it |
| Millisecond comparisons not wrap-safe | LED display stuck after ~49 days uptime | `wrapping_sub` comparisons |

## Performance

The curved modes call `exp_curve` every sample. It used fixed-point 64-bit
divisions and, for negative curves, two 64-bit reciprocals. AVR has no hardware
divider, so these cost thousands of cycles. The rewrite:

- Uses the symmetry f(−c, x) = 1 − f(c, 1 − x), which removes the reciprocals entirely.
- Computes the ratio with one 32-bit division after scaling both terms.
- Interpolates the `2^x` table in u32 math instead of a 64-bit fixed `lerp`.

Accuracy is unchanged: the worst error against the exact formula is 5.0 LSB (was 5.1).

Compute time per sample, measured in simavr (budget 480 µs):

| Mode | Before (avg / worst) | After (avg / worst) |
|---|---|---|
| ACRC | 248 / 435 µs | 161 / 278 µs |
| ACRC loop | 318 / 438 µs | 196 / 285 µs |
| ADSR | 64 / 224 µs | 84 / 216 µs (now also runs pings) |
| AHRD loop | 93 / 184 µs | 90 / 223 µs |

The curved modes previously had only ~40 µs of headroom per sample. They now have
~200 µs.

## Features added

- **Long time range**: hold the button for 1 s to toggle a ×10 range (up to 100 s per
  stage). The LEDs blink all four for the long range, or the outer two for the normal
  range. The setting is saved.
- **Mode changes on release**: a short click changes mode, so a long press can be
  distinguished from it. Holding the button at power-on still erases the saved
  settings.
- **ADSR ping**: Trig without a gate (see the bug list above).
- **ADSR skips a no-op decay** when sustain is at maximum.

## Ideas not implemented

- DAC channel B is free in firmware but its pin isn't routed. A board revision could
  add an inverted or second envelope output.
- In ACRC mode, a gate edge still repeats the previous sample once while computing the
  inverse. With the new headroom the inverse and the next sample would now fit in one
  period.

## Testing

- **Host unit tests** (`modules/Envelope/tests/math`) cover:
  - curve accuracy against the exact formula and against the original implementation;
  - monotonicity and endpoints;
  - inverse round-trips;
  - the ring-buffer head search for every head position over several laps.

  ```
  cd modules/Envelope/tests/math && cargo test
  ```
- **Simulator** (`modules/Envelope/tools/sim`) runs the firmware in simavr with
  scripted gate, trigger, button and knob input. It logs every DAC sample, the LED and
  aux changes, and the per-sample compute time.
  ```
  tools/sim/run_sim.sh 13 tools/sim/scenarios/all_modes.txt
  tools/sim/analyze.py tools/sim/out 0 3.5 ADSR 3.6 6.5 ACRC 6.6 9 ACRC-loop 9.1 11 AHRD
  ```
  simavr's SPI is ~100× slower than real hardware, so judge CPU headroom from the
  compute-time log, not from gaps in the DAC log.
