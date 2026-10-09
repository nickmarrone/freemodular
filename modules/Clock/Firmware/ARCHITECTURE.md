# FM Clock — Architecture, Review & Roadmap

Developer-facing notes for the Clock module firmware: how it works, the bugs and
performance problems found in the original firmware (and how they were fixed), a
comparison with popular Eurorack clocks, and the feature roadmap.

**Status (Oct 2026):** All Tier 1 and Tier 2 items below are implemented. Tier 3 needs
hardware. Measurements were taken in simavr (see [§7 Testing](#7-testing)). They have
**not** been verified on a physical module yet.

---

## 1. Hardware overview

| Item | Detail |
|---|---|
| MCU | Arduino Nano (ATmega328P, 16 MHz, 32 KB flash, 2 KB RAM, 1 KB EEPROM) |
| Display | 128×64 SSD1306 OLED, hardware SPI (SCK=D13, MOSI=D11, DC=A1, CS=A2, RST=A0) |
| Encoder | EC11 with push switch. A/B on D8/D9 (PCINT0 interrupt), switch on A4 |
| Play/pause button | A3 |
| Outputs | 8 gates = all of PORTD (D0–D7). Each drives a jack through 1 kΩ, plus an LED. 0/5 V |
| Power | +5 V only (≈25 mA typical) |
| Size | 8 HP, two stacked PCBs (front: jacks/LEDs/controls; back: Nano/power) |

**Spare resources**
- **J13** on the back PCB breaks out **A5, A6, A7**. This is the hook for future inputs.
  A5 is a full digital pin with pin-change interrupt. **A6 and A7 are analog-only.**
- **D10** (SPI SS) is unconnected but must stay an output for SPI master mode, so it is a
  free scope/debug pin. **D12** (MISO) is also unconnected.
- **Timer0 and Timer2 are now free.** Timer1 drives the clock engine.

**Flash budget.** The release build (what `build.py` ships) is now **30,652 B**. That
leaves ~1.6 KB below the 32,256 B usable with the Optiboot bootloader. The original was
30,858 B with fewer features. The `dev` profile (debug assertions and overflow checks)
does not fit and never fit under the bootloader limit (the original was 32,300 B). Flash
with `cargo run --release`.

---

## 2. Firmware architecture

### 2.1 Module map

```
src/
  main.rs            setup, minimal panic handler, encoder ISR, UI superloop
  clock.rs           config types + clock engine (planner in main loop, executor in TIMER1 ISR)
  eeprom.rs          raw-register EEPROM access, wear-levelled live config, 8 preset slots,
                     migration of the old (v1) format
  menu/
    menu_state.rs    pages, editing states, list items, MenuUpdate redraw deltas
    menu_logic.rs    input -> state/config changes; returns (MenuUpdate, ConfigChange)
    utils.rs         division stepping (powers of two / single steps / special outputs)
    menu_graphics/   one renderer per page; list pages share one table-driven renderer
  display_buffer.rs  MiniBuffer<W,H> + non-generic Canvas drawing code (compiled once)
  font.rs            PROGMEM bitmap fonts (custom code page)
  render_numbers.rs  number/word formatting, PROGMEM word table
  random.rs          PROGMEM random table (screensaver)
```

### 2.2 Clock engine (`clock.rs`)

The engine is split so that **output timing never depends on the UI**:

- **Planner** (main loop): `apply_config(config, realign_mask)` turns each channel's
  settings into integer constants (`ChannelParams`). It runs whenever a setting changes,
  and all slow math (division, u64) happens here.
- **Executor** (`TIMER1_COMPA`, 8 kHz = 125 µs resolution): it writes PORTD *first*, so the
  latency is constant, and then advances the channels.

**Phase accumulators.** The master accumulator gains `bpm10` per tick and wraps at
`MASTER_WRAP = 60 s × 8000 × 10` (one beat). A channel running at `num/den` periods per
beat gains `bpm10 × num` per tick and wraps at `MASTER_WRAP × den`. All channels therefore
stay **exactly** phase-locked to the master, with no drift and no rounding accumulation.
Tempo changes are seamless, because positions are preserved as fractions of a period.

- ×n → `n/1`; ÷n → `1/n`.
- Triplet multiplies the rate by 3/2, dotted by 2/3.

**Event-driven executor.** Each channel only stores `acc`, `next` and `inc` on the hot
path. Every tick it does `if acc >= next { process_events() } acc += inc`.
`process_events` walks the three events of a period (pulse on → pulse off → wrap). On
wrap it advances the Euclidean Bresenham bucket and rolls probability (xorshift16). Odd
periods use the swing window `[swing_start, swing_end)` and even ones `[0, high)`.

**Realignment.** When a channel's rate, phase or Euclidean pattern changes,
`realign_channel` computes where it would be had it always run with the new settings:

- position = master travel × num + phase offset; period index = position / wrap.
- From the period index it derives swing parity and the Euclidean step (including rotation).
- The u64 math runs outside the critical section. Inside it, the result catches up on
  any ticks that elapsed meanwhile and is published atomically together with the new
  params.
- It never starts a pulse mid-way, which would produce a spurious trigger.

**Special outputs** (division values below ÷64): `STOP` (high while stopped), `RUN`
(high while running), `RST` (5 ms trigger on every start). These are bit masks OR'd into
the output, so they cost nothing per channel.

**Stopping**: `stop(mode)` stops now, on the next beat, or on the next bar (4 beats),
exactly on the line. Pulses already high when stopping are held for ≥5 ms, so there are
no runt triggers.

### 2.3 Main loop (`main.rs`)

The main loop only does UI work: play button, `update_menu`, applying `ConfigChange`
(`Params` → `apply_config(.., 0)`, `Realign(mask)` → `apply_config(.., mask)`), the
debounced EEPROM save, and partial redraws. `millis()` comes from the engine (one count
every 8 ticks) and is compared with `wrapping_sub`, so a 49-day wrap is harmless.

### 2.4 Persistence (`eeprom.rs`)

```text
0 ................................ 424 ...................................... 1024
| 5 wear-levelled live blocks       | preset slot 1 | ... | preset slot 8 (75 B) |
  block = [MAGIC 0xC2, version, ClockConfig (75 B)]
```

- **Boot**: pick the highest-version block with a valid magic and validate it. If no
  valid block is found, try importing a config saved by the old firmware. Then copy to
  the next block, writing data first and header last, so torn writes are never chosen.
- **Edits**: `mark_dirty`, then the whole config is written 1.5 s after the last change.
  `write_byte` skips bytes that are unchanged, so only changed cells wear.
- Registers are accessed directly, with interrupts disabled for the 4-cycle EEMPE→EEPE
  window. avr-hal didn't do this, which would race with the 8 kHz ISR. This also saves
  ~4.6 KB of flash compared with avr-hal's generic `Eeprom`.
- **Bump `MAGIC` whenever the `ClockConfig` layout changes.**

### 2.5 UI

```
                [Global page: Stop (Now/Beat/Bar) · Load 1-8 · Save 1-8]
                   ▲ scroll up            │ scroll down past the last row
                   │                      ▼
   ┌──────────► [BPM page]   click: edit BPM → click: edit tenths → click: done
   │            long-press: TAP mode (clicks = taps; turn or 3 s idle exits)
   │               │ scroll down
   │               ▼
   │      [Main: 2 pages × 4 channels]  click: fast-edit division (powers of 2 / STOP RUN RST)
   │  scroll above ch1 │ ▲ long-press / Exit
   └───────────────────┘ │ long-press
                         ▼
   [Channel page: Tempo · Tuplet · PulseW · Phase · Swing · Prob · Steps · Fill · Rotate · Exit]
      (STOP/RUN/RST channels only show Tempo + Exit)

  Play: start (restarts from beat 1) / stop (per Stop mode; press again to cancel a pending stop)
  Hold play 2 s → "Reset?" page: click to factory reset, turn to cancel
  5 s idle → screensaver (fills one block per beat); any input wakes it
```

- **Load/Save** take two clicks: one to select the slot, one to commit.
- **Encoder acceleration:** detents < 40 ms apart count ×4 for BPM, PW, probability and
  phase/swing.
- The main page shows tuplets as a suffix: `x4t` (triplet), `_4\` (dotted, where `\`
  renders as `.`).
- Fonts use a custom code page: `;`=`-`, `` ` ``=`+`, `_`=`/`, `^`=`%`, `\`=`.`.
  The `.` glyph was added for fine BPM and dotted notes.

---

## 3. Bugs found in the original firmware

All of these are **fixed**. Line numbers refer to the original commit `06c98a8`.

| # | Bug | Impact | Fix |
|---|---|---|---|
| 1 | `submenu.rs:30` redrew the edited value with `scroll` hard-coded to 0 | Editing Phase/Swing didn't visibly update. With scroll = 1, PulseW was painted over the wrong row. The resulting `blit` error went through `assert_ok` = UB in release | Generic list renderer uses the real row |
| 2 | `clock.rs:205` `period × pulse_width` overflowed u32 | Garbage pulse widths for ÷32 at < 45 BPM or ÷64 at 50–88 BPM when PW ≳ 67% | Planner computes `wrap/100 × pw` (verified: ÷64 @ 20 BPM, PW 99%) |
| 3 | 5 ms trigger not scaled in the ÷10 low-resolution path | 50 ms TRIG pulses on slow channels | Single resolution, no fallback path |
| 4 | EEPROM validation accepted `division == 0` | `% 0` panic on boot, every boot (boot loop) | `is_valid` rejects 0 and −1 |
| 5 | EEPROM scan read slot 1015, which is never written | Out-of-bounds read → UB/panic with foreign EEPROM data | New layout plus magic byte; foreign data ignored (sim-tested with random EEPROM) |
| 6 | `assert_ok()` = `unwrap_unchecked` on real, recoverable errors | UB | Display/EEPROM paths ignore errors |
| 7 | Phase-wrap `>` vs `>=` | 1-sample edge case | New engine |
| 8 | A big BPM jump caused a burst of rollovers | Glitch pulses, divided channels skipping ahead | Accumulator positions are tempo-independent |
| 9 | Pausing cut pulses short | Runt triggers | Pulses held ≥5 ms |
| 10 | u32 ms wrap (49.7 days) | Clock glitch, stuck screensaver | `wrapping_sub` everywhere; µs/u64 time removed |
| 11 | UX: pausing jumped to the BPM page; hold-play reset without confirmation; manual said 2 s but code used 2.5 s; edits lost within 5 s of power-off | — | Page kept; confirmation page; 2 s; save 1.5 s after the last change |
| 12 | avr-hal EEPROM write didn't mask interrupts between EEMPE and EEPE | Writes can silently fail if an interrupt lands in the window (latent before, likely with an 8 kHz ISR) | Raw access inside `interrupt::free` |

Bugs found (and fixed) in the *new* code by the tests and the simulator:
- Realigning a STOP/RUN/RST channel divided by zero.
- `start()` realigned while still flagged as running, which suppressed the first pulses
  after a factory reset.
- A division change mid-pulse produced a partial trigger.
- The first tap was measured from when tap mode was entered.

---

## 4. Performance

### 4.1 Original design
Outputs were computed by polling in the superloop. That meant ~20 u32 divisions per loop,
u64 `micros()` math, and a busy-wait for TCNT0. **Every** edge was late by the loop time,
which varied and stretched to tens or hundreds of ms during display redraws and EEPROM
writes.

### 4.2 Measured now (simavr, 16 MHz)

| Metric | Result |
|---|---|
| Executor ISR CPU load | **~27%** (first version, which recomputed every channel every tick, was 82%) |
| ISR worst case | 589 cycles of the 2000-cycle tick |
| Edge timing vs ideal grid over 17.5 s of menu navigation and edits | ≤ 1,628 cycles (102 µs, under one tick), identical on all channels (no inter-channel skew). Non-integer periods alternate between neighbouring ticks with an exact average |
| Tempo glide 128 → 138 BPM | Continuous, no glitches or extra edges |
| Quantized stop (bar) | Stops exactly on the bar line; restart is on the beat |

### 4.3 Flash optimizations that paid for the features
- Raw EEPROM access instead of avr-hal `Eeprom` (−4.6 KB).
- Minimal panic handler (−1.3 KB).
- Drawing code moved into a non-generic `Canvas`, so it is compiled once rather than once
  per `MiniBuffer` size.
- `embedded-graphics` `DrawTarget` dropped.
- The engine's statics live in `.bss` (no `.data` initializer).
- PROGMEM label and word tables.
- `inline(never)` on cold, large functions.
- The ISR loop indexes with `get_unchecked`.

### 4.4 Remaining ideas
- Render speed: the UI is ~30% slower than before because the ISR takes ~27% of the CPU.
  It is still responsive. The SPI transfer itself is fast on hardware.
- `Canvas::fill` is pixel-by-pixel (small and simple). A byte-wise fill is faster if
  redraws ever feel sluggish.

---

## 5. Comparison with popular Eurorack clocks

| | **FM Clock (now)** | **ALM Pam's Pro Workout** | **Make Noise Tempi** | **4ms SCM / SCM+** |
|---|---|---|---|---|
| Outputs | 8 gates | 8 (gates *or* CV waveforms) | 6 gates | 8 gates |
| Ratios | ×64 … ÷64, plus triplet/dotted | ×192 … ÷16384 | ×16 … ÷256 | ×1 … ×8 (×32 w/ breakout) |
| Non-integer ratios | ✓ (3:2, 2:3) | ✓ | ✓ | — |
| Swing / shuffle | ✓ per channel | ✓ ("Flex" microtiming) | via phase/nudge | ✓ Slip / Shuffle |
| Pulse width | ✓ | ✓ | — | ✓ (breakout) |
| Phase | ✓ | ✓ | ✓ | — |
| Euclidean | ✓ steps/fill/rotate | ✓ | — | — |
| Probability / skip | ✓ | ✓ | Mutate | ✓ Skip |
| Run / stop / reset outputs | ✓ STOP, RUN, RST | ✓ | ✓ | — |
| Quantized stop | ✓ beat/bar | — | — | — |
| LFO / envelope outputs | — (digital pins) | ✓ | — | — |
| External clock / reset in | — (Tier 3) | ✓ | ✓ | ✓ |
| CV inputs | — (Tier 3) | 4 (+4 w/ Axon-1) | State select + Mod | Slip, Rotate (+3) |
| Tap tempo | ✓ | — | ✓ | — |
| Presets | ✓ 8 slots | 7 banks × 8 | 64 states | Save Clock |
| HP | 8 | 8 | 8 | 4 / 12 |

FM Clock now matches or beats the gate-domain feature set of these modules. The
remaining gap is **inputs** (external clock, reset, CV), which needs hardware (§6.3).
Waveform outputs are out of scope for this hardware: the outputs are bare digital pins.

Sources: [ModularGrid – Pamela's PRO Workout](https://modulargrid.net/e/modules/view/41596),
[Turramusic – ALM034](https://www.turramusic.com.au/collections/modules-1/products/alm-busy-circuits-pamelas-pro-workout-alm034-1),
[Schneidersladen – Make Noise Tempi](https://schneidersladen.de/en/make-noise-tempi),
[KVR – Tempi](https://www.kvraudio.com/product/tempi-by-make-noise-music),
[Clockface Modular – 4ms SCM+](https://en.clockfacemodular.com/products/4ms-shuffling-clock-multiplier).

---

## 6. Feature roadmap

### 6.1 Tier 1 — done
1. Bug fixes and the ISR engine (§2.2, §3).
2. **Tap tempo:** long-press on the BPM page; the average of the last 4 taps sets the tempo.
3. **Per-channel probability:** 0–100%, rolled once per period.
4. **Per-channel Euclidean rhythms:** steps (Off, 2–32), fill, rotate.
5. **Special outputs:** STOP, RUN, RST.
6. **Encoder acceleration.**

### 6.2 Tier 2 — done
7. **Presets:** 8 slots on the Global page.
8. **Tuplets:** triplet (×3/2) and dotted (×2/3) per channel.
9. **Quantized stop:** now / next beat / next bar.
10. **Safer factory reset:** a confirmation page.
11. **Fine BPM:** tenths of a BPM; the range widened to 20.0–300.0 BPM.
12. **Settings migration** from the old EEPROM format.

### 6.3 Tier 3 — needs hardware (J13 header, jacks, faceplate rev)

| Feature | Pin | Notes |
|---|---|---|
| **External clock in** | A5 (digital, PCINT1) | Timestamp edges, PPQN setting on the Global page, smooth the period. Feed `bpm10` (or a fractional tempo) to the planner. The accumulator engine makes sync straightforward: phase-correct the master toward each incoming edge |
| **Reset / run in** | A6 (ADC only) | Free-running ADC + threshold with hysteresis. Reset = `clock::start`; run = start/stop |
| **CV in** | A7 (ADC only) | BPM, or one assignable channel parameter |

All of these inputs need protection (a BJT buffer, or a series resistor plus clamps) and
panel space. A small 2–4 HP expander on J13 avoids redesigning the 8 HP panel.

---

## 7. Testing

**Host unit tests** (`modules/Clock/tests/engine`) compile `clock.rs` and `menu/utils.rs`
for the host and drive the executor tick by tick. Hardware access is behind
`cfg(target_arch = "avr")`. They cover:
- default divisions;
- tuplets locked to the master;
- swing and phase;
- Euclidean patterns with rotation;
- realignment mid-run;
- tempo changes keeping lock;
- quantized stop and the STOP/RUN/RST outputs;
- pulse-width limits;
- probability;
- slow-division overflow;
- division stepping.

```
cd modules/Clock/tests/engine && cargo test
```

**Simulator** (`modules/Clock/tools/sim`) runs the real firmware ELF in simavr. It logs
every output change with its cycle timestamp, scripts encoder and button input, loads and
saves EEPROM images, and measures ISR load.

```
tools/sim/run_sim.sh 8 tools/sim/scenarios/global_page_presets_stop.txt
tools/sim/analyze.py tools/sim/out/portd.log
```

Caveats:
- simavr's SPI takes ~100 µs per byte (real: ~1 µs), so simulated redraws are ~100× slower
  than on hardware. Scripted button presses should be held ≥250 ms.
- simavr re-applies pull-ups on PORTC writes; the harness re-asserts the held button levels.
