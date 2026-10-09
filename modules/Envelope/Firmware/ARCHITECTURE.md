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
| Aux gate out | A3. The jumpers on A1/A2 choose its default mode (end-of-rise, end-of-fall, non-zero, or follow-gate); a hidden setting overrides them |
| Spare | D9, D12, A0 (net only), DAC channel B (pin not routed). With the `profile` feature, D9 and A0 time the envelope math and the UI work |

## Firmware architecture

- **Sample clock**: TIMER2 CTC at exactly **2083.3 Hz (480 µs)**. The ISR only raises
  the DAC chip-select, which latches the sample the main loop already shifted in. This
  gives jitter-free output with one sample of latency.
- **Main loop**: as soon as the previous sample has been latched, it reads the gate and
  the queued trigger, calls `envelope::update`, and shifts the result into the DAC with
  CS held low, then updates the aux output. After that it does the UI work: reading
  the knobs, the button and hidden settings, and the LEDs. It only starts the UI work
  if TIMER2's counter shows enough time left (`UI_MAX_TICKS`) before the next sample,
  so the UI never delays a sample.
- **ADC**: free-running, interrupt driven, cycling through 4 channels (fm-lib
  `async_adc`).
- **Time**: each stage is a u32 phase that advances by `u32::MAX / steps` per sample.
  `steps` comes from the knob through a piecewise exponential response: up to 10 s per
  stage, or 100 s in the long range.
- **Modes** (`envelope/`), in button order:
  1. `adsr.rs` — ADSR, with optional curves (hidden settings).
  2. `acrc.rs` — AR with variable curve (ACRC).
  3. `acrc.rs` — the looping version of ACRC.
  4. `ahrd.rs` — looping attack-hold-release-delay.
  5. `slew.rs` — slew limiter.
  6. `burst.rs` — burst of AR pulses.
  7. `random.rs` — looping AR with random times and heights.
  8. `ball.rs` — bouncing ball.

  Each mode is a function from (mode state, phase `time`, inputs, knobs) to (output,
  stage changed). `aux.rs` maps each mode's stage to the flags the aux modes show, and
  `ui_show_stage` maps it to the LEDs.
- **Hidden settings and pickup** (`settings.rs`): hardware independent and unit tested.
  `Editor` notices which knob was turned while the button was held. `Pickup` keeps
  that knob's parameter at its old value until the knob is turned back through it.
- **Curves** (`exponential_curves.rs`): `(2^(16xc) − 1) / (2^(16c) − 1)` from a
  256-entry `2^x` table. The inverse, used to restart a stage from the current level
  without a jump, uses a `log2` table.
- **Persistence**: 6 bytes through fm-lib's `WearLevelledEepromWriter`: mode (bits 0–2)
  and long range (bit 7), a format marker, the two ADSR curves, gate behaviour, and aux
  mode (0xFF = use the jumpers). Only changed bytes are written. A save from older
  firmware (1 byte, no marker) keeps its mode and range; the other settings start at
  their defaults.

## Controls

| Action | Effect |
|---|---|
| Click | Next mode. Modes 1–4 light one LED; modes 5–8 light all but one |
| Hold 1 s and release, no knob turned | Toggle the 10 s / 100 s time range (previewed on the LEDs at 1 s) |
| Hold and turn knob 1 | ADSR attack curve (middle = linear) |
| Hold and turn knob 2 | ADSR decay and release curve (middle = linear) |
| Hold and turn knob 3 | Gate behaviour for ADSR and AR: continue / reset / legato / cycle |
| Hold and turn knob 4 | Aux mode: end of rise / end of fall / non-zero / follow gate / end-of-rise pulse / end-of-fall pulse |
| Hold at power-on | Erase saved settings |

While a setting is being edited, the LEDs show its value: one LED per option (the two
pulse aux modes light the top two or bottom two LEDs). For curves, the LEDs show which
quarter of the knob is selected, and the middle two LEDs mean linear. Knob turns only
count as edits after the button has been held for 250 ms, so quick clicks always
change the mode even with CV patched into a knob.

Gate behaviours:
- **Continue** (default, as before): a new gate or trigger restarts the attack from
  the current level.
- **Reset**: restart from zero.
- **Legato**: no restart while the envelope is still rising, decaying or sustaining. A
  gate only takes over holding open an envelope that a trigger started.
- **Cycle**: while the gate is held, ADSR loops attack → decay and AR loops attack →
  release.

New modes:

| Mode | Knob 1 | Knob 2 | Knob 3 | Knob 4 | Gate | Trig |
|---|---|---|---|---|---|---|
| Slew | Input | Rise time | Fall time | Shape (linear → exponential) | Hold the output | Drop to zero |
| Burst | Pulse time | Pulses (1–16) | Heights: shrink ← level → grow | Curve | Fire; repeat while held | Fire |
| Random loop | Time | Time randomness (±2 octaves) | Height randomness | Curve | Pause at zero | Restart |
| Bouncing ball | Drop time | Bounciness | Gravity: straight → parabola → sharper | Drop height | Lift and hold; release drops | Drop |

Aux in the new modes: in slew, rising counts as the gate, and settling counts as the
end of both rise and fall. In burst and ball, the end of every pulse or bounce is an
end of fall, so the end-of-fall pulse fires once per pulse or bounce.

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
  stage). Since round 2 it toggles on release, and only if no knob was turned. The LEDs blink all four for the long range, or the outer two for the normal
  range. The setting is saved.
- **Mode changes on release**: a short click changes mode, so a long press can be
  distinguished from it. Holding the button at power-on still erases the saved
  settings.
- **ADSR ping**: Trig without a gate (see the bug list above).
- **ADSR skips a no-op decay** when sustain is at maximum.

## Round 2 (Oct 2026): new features

Features chosen after comparing the module with Maths, Rampage, Peaks, Stages and
Quadrax. The main gaps were: a linear-only ADSR, no slew, no burst, aux configurable
only by jumpers, and fixed retrigger behaviour. Everything above under Controls was
added, plus:

- AR mode no longer repeats the previous sample on gate edges. The restart and the
  next sample are computed in the same period.
- `exp_curve_inverse` uses 32-bit math instead of a 64-bit multiply and division, with
  the same accuracy. This is what makes the previous item fit.
- Release builds use `opt-level = "s"`. The image is ~40% smaller with the same
  worst-case compute time in simavr, and the firmware no longer fit in flash at
  opt 3. It is now ~24 KB of the ~30 KB available.
- Bounds-check panics pull in all of `core::fmt` (several KB). Code indexed by runtime
  values uses `& 3` or `.get()` to avoid them.

Envelope math per sample, measured in simavr with the `profile` feature (budget 480 µs
including the ~110 µs UI pass, which only runs in the time left over):

| Mode | avg / worst |
|---|---|
| ADSR (linear) | 68 / 128 µs |
| ADSR (curved) | — / 310 µs on gate edges; 352 µs when cycling restarts the attack |
| ACRC | 147 / 309 µs |
| ACRC loop | 178 / 185 µs |
| AHRD loop | 64 / 184 µs |
| Slew | 99 / 121 µs |
| Burst | 135 / 242 µs |
| Random loop | 200 / 313 µs |
| Bouncing ball | 115 / 189 µs |

## Ideas not implemented

- DAC channel B is free in firmware but its pin isn't routed. A board revision could
  add an inverted or second envelope output.
- Clock sync: loop modes could measure the Trig period and lock to it.

## Testing

- **Host unit tests** (`modules/Envelope/tests/math`) cover:
  - curve accuracy against the exact formula and against the original implementation;
  - monotonicity and endpoints;
  - inverse round-trips;
  - the ring-buffer head search for every head position over several laps;
  - settings encoding, legacy saves, the hidden-settings editor and pickup;
  - every new mode and gate behaviour, driven sample by sample through
    `envelope::update` (`mode_tests.rs`).

  ```
  cd modules/Envelope/tests/math && cargo test
  ```
- **Simulator** (`modules/Envelope/tools/sim`) runs the firmware in simavr with
  scripted gate, trigger, button and knob input. It logs every DAC sample, the LED and
  aux changes, and the per-sample compute time.
  ```
  tools/sim/run_sim.sh 13 tools/sim/scenarios/all_modes.txt
  tools/sim/analyze.py tools/sim/out 0 3.5 ADSR 3.6 6.5 ACRC 6.6 9 ACRC-loop 9.1 11 AHRD
  FEATURES=profile tools/sim/run_sim.sh 21 tools/sim/scenarios/new_modes.txt
  ```
  Scenarios: `all_modes`, `long_time_range`, `hidden_settings` (gesture, pickup, aux
  pulse), `adsr_curves_gate` (curves and the four gate behaviours), and `new_modes`.

  simavr quirks:
  - Its SPI is ~100× slower than real hardware. It drops samples (so envelopes run
    slow) and inflates the compute log. Build with `FEATURES=profile` to time the
    envelope math (pin D9) and the UI pass (pin A0) directly.
  - It applies ADMUX at once, while hardware waits for the next conversion. The
    firmware sets ADMUX two conversions ahead for hardware, so the harness feeds
    each knob's voltage to the next channel to compensate.
