# Drift firmware architecture

## Main loop and timing

- TIMER0 interrupts every 400 µs (2.5 kHz). When a sample is queued, the interrupt
  raises the DAC's CS pin, which latches it.
- The main loop computes the next sample as soon as the last one has been latched.
  It writes that sample over SPI but holds CS low, so the sample comes out on the
  next timer tick with no jitter.
- The ADC free-runs over A4, A5, ADC6 and ADC7 (`fm_lib::async_adc`). The `cv`
  array that the algorithms see holds those inputs in that order: speed CV,
  texture CV, speed knob and texture knob.
- The DIP switches (D5, D4) choose the algorithm at power-up (`drift.rs`).

## Speed

`shared::get_delta_t` turns knob + CV (+ an offset, for Bezier's random timing)
into a phase increment for a 32-bit counter, at 1 V/oct from 1/40 Hz to about
100 Hz. It uses a 16-entry 2^(j/16) table in progmem, shifts by whole octaves and
interpolates linearly between sixteenths of an octave. There are no divisions.
Perlin, Bezier and the LFO wrap their counters rather than restarting them, so no
fraction of a sample is lost per cycle.

## Algorithms

- **Perlin** (`perlin.rs`): two 1D gradient-noise layers, the second running 4x
  faster. Texture blends the second layer in. Each segment value stays within
  ±0.25, so the base × (4 − blend) + octave × blend mix fits I1F15. The host tests
  check this exhaustively.
- **Bezier** (`bezier.rs`): cubic easing between random points. Texture sets the
  curve shape (by which side of center the knob is on) and how far the segment
  length varies at random (from a triangular distribution, `random.rs`).
- **Brownian** (`brownian.rs`): a random walk. A first-order filter smooths it,
  with a 16.16 fixed-point state so that it settles on the target exactly. The
  texture knob sets the smoothing exponentially, from a ~1.6 s time constant down
  to ~3 ms; at full texture the filter is bypassed.
- **LFO** (`lfo.rs`): a triangle whose peak position is set by texture. The output
  follows a line toward the peak (or toward 0 at the end of the cycle). When
  texture moves, the line is re-aimed from the current output, so the shape
  changes at once without jumps and the cycle length stays the same.

## CPU budget

The sample budget is 400 µs. Compute time per sample measured in simavr:

| mode     | before | after (avg / max) |
| -------- | ------ | ----------------- |
| Perlin   | ~363   | 189 / 216         |
| Bezier   | ~252   | 78 / 167          |
| Brownian | ~27    | 31 / 49           |
| LFO      | ~260   | 108 / 122         |

Most of the old cost was a 64-bit division in `get_delta_t` (and a second one in
the LFO).

## Testing

- `tests/math`: host unit tests for the speed math and every algorithm (range,
  continuity, smoothing convergence, LFO cycle length).
  Run `cargo test` there; debug mode also catches arithmetic overflow.
- `tools/sim`: a simavr harness. `MODE=lfo tools/sim/run_sim.sh 12
  tools/sim/scenarios/lfo_rates.txt`, then `tools/sim/analyze.py tools/sim/out`.
  simavr's SPI is ~100x slower than the real one. Before the speedups, that made
  the simulator skip every other sample: frequencies read half and the period
  read 808 µs. The harness also feeds each ADC input one channel early, to make up
  for simavr applying ADMUX changes immediately.
