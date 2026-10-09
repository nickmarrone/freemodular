#![allow(incomplete_features)]
#![no_std]
#![no_main]
#![feature(abi_avr_interrupt)]
#![feature(asm_experimental_arch)]
#![feature(int_roundings)]
#![feature(adt_const_params)]
#![feature(generic_const_exprs)]
#![feature(const_trait_impl)]
#![feature(cell_update)]

use core::arch::asm;
use core::{cell::Cell, panic::PanicInfo};

use arduino_hal::hal::port;
use arduino_hal::port::mode::Output;
use arduino_hal::port::Pin;
use arduino_hal::{hal::port::PB0, prelude::*, Peripherals};
use avr_device::interrupt::{self, Mutex};
use embedded_hal::digital::v2::OutputPin;
use envelope::{ui_show_mode, ui_show_stage, update, EnvelopeMode};
use fm_lib::{
    async_adc::{
        handle_conversion_result, init_async_adc, new_async_adc_state, AsyncAdc, GetAdcValues,
    },
    asynchronous::{assert_interrupts_disabled, unsafe_access_mutex},
    asynchronous::{AtomicRead, Borrowable},
    button_debouncer::{ButtonWithLongPress, LongPressButtonState},
    eeprom::WearLevelledEepromWriter,
    handle_system_clock_interrupt,
    mcp4922::{DacChannel, MCP4922},
    system_clock::{ClockPrecision, GlobalSystemClockState, SystemClock},
};
use ufmt::uwriteln;

use crate::aux::{aux_flags, AuxOutput};
use crate::envelope::{set_long_time_range, EnvelopeState, GateState, Input};
use crate::settings::{AuxMode, Editor, Pickup, Settings, SAVED_SIZE};

mod aux;
mod envelope;
mod exponential_curves;
mod settings;

static SYSTEM_CLOCK_STATE: GlobalSystemClockState<{ ClockPrecision::MS16 }> =
    GlobalSystemClockState::new();
handle_system_clock_interrupt!(&SYSTEM_CLOCK_STATE);

const UI_SHOW_ENVELOPE_MODE_MS: u32 = 2000;
#[derive(PartialEq, Eq, Clone, Copy)]
enum DisplayMode {
    /// Blink `pattern` on the LEDs until `until`, then go back to showing the stage
    Blink { pattern: u8, until: u32 },
    ShowEnvelopeSegment,
}

/// LED pattern shown after toggling the time range: all four LEDs for the long
/// (100 s) range, the outer two for the normal (10 s) range
fn ui_show_time_range(long: bool) -> u8 {
    if long {
        0xF0
    } else {
        0x90
    }
}

#[inline(never)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let dp = unsafe { arduino_hal::Peripherals::steal() };
    let pins = arduino_hal::pins!(dp);
    let mut serial = arduino_hal::default_serial!(dp, pins, 57600);
    serial.flush();
    serial.write_byte(b'\r');
    serial.write_byte(b'\n');
    serial.write_byte(b'\r');
    serial.write_byte(b'\n');
    if let Some(location) = info.location() {
        uwriteln!(
            &mut serial,
            "Panic occurred in file '{}' at line {}",
            location.file(),
            location.line()
        )
        .unwrap_infallible();
    } else {
        uwriteln!(&mut serial, "Panic occurred").unwrap_infallible();
    }

    let short = 100;
    let long = 500;
    let mut led = pins.d13.into_output();
    loop {
        for len in [short, long] {
            for _ in 0..3u8 {
                led.set_high();
                arduino_hal::delay_ms(len);
                led.set_low();
                arduino_hal::delay_ms(short);
            }
        }
    }
}

/**
 Pin-change interrupt handler pin d3 (external interrupt 1)
*/
#[avr_device::interrupt(atmega328p)]
fn INT1() {
    assert_interrupts_disabled(|cs| {
        QUEUED_TRIGGER.borrow(cs).set(true);
    });
}

#[avr_device::interrupt(atmega328p)]
fn TIMER2_COMPA() {
    let dp = unsafe { arduino_hal::Peripherals::steal() };

    assert_interrupts_disabled(|cs| {
        if DAC_WRITE_QUEUED.borrow(cs).get() {
            DAC_WRITE_QUEUED.borrow(cs).set(false);
            dp.PORTB
                .portb
                .modify(|r, w| unsafe { w.bits(r.bits()) }.pb2().set_bit());
        } else {
            #[cfg(feature = "debug")]
            DEBUG_SKIPPED_WRITE_COUNT.borrow(cs).update(|x| x + 1);
        }
    });
}

/// Enable external interrupts for INT0 (digital pin 2)
fn enable_external_interrupts(dp: &Peripherals) {
    // set pin d3 as an input
    dp.PORTD
        .ddrd
        .modify(|r, w| unsafe { w.bits(r.bits()) }.pd3().clear_bit());

    // enable pullup resistors for pin d3
    dp.PORTD
        .portd
        .modify(|r, w| unsafe { w.bits(r.bits()) }.pd3().set_bit());
    // enable external interrupt 1
    dp.EXINT.eimsk.write(|r| r.int1().set_bit());
    // trigger interrupt 1 on falling edge (trigger inputs are inverted)
    dp.EXINT.eicra.write(|r| r.isc1().val_0x02());
}

static GLOBAL_ASYNC_ADC_STATE: AsyncAdc<4> = new_async_adc_state();

#[avr_device::interrupt(atmega328p)]
fn ADC() {
    handle_conversion_result(&GLOBAL_ASYNC_ADC_STATE);
}

impl EnvelopeState {
    fn cycle_mode(self) -> Self {
        Self {
            mode: self.mode.next(),
            time: 0,
            last_value: 0,
            artificial_gate: false,
        }
    }
}

static DAC_WRITE_QUEUED: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));
static QUEUED_TRIGGER: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));

#[cfg(feature = "debug")]
static DEBUG_SKIPPED_WRITE_COUNT: Mutex<Cell<u8>> = Mutex::new(Cell::new(0));

#[arduino_hal::entry]
fn main() -> ! {
    let dp = arduino_hal::Peripherals::take().unwrap();

    enable_external_interrupts(&dp);

    let pins = arduino_hal::pins!(dp);
    let mut adc = arduino_hal::Adc::new(dp.ADC, Default::default());
    let a4 = pins.a4.into_analog_input(&mut adc);
    let a5 = pins.a5.into_analog_input(&mut adc);
    let btn_pin = pins.d8.into_pull_up_input();
    let gate_pin = pins.d2.into_pull_up_input();
    let config_pin_1 = pins.a2.into_pull_up_input();
    let config_pin_2 = pins.a1.into_pull_up_input();

    let (mut spi, d10) = arduino_hal::spi::Spi::new(
        dp.SPI,
        pins.d13.into_output(),        // Clock
        pins.d11.into_output(),        // MOSI
        pins.d12.into_pull_up_input(), // MISO
        pins.d10.into_output(),        // CS
        arduino_hal::spi::Settings {
            data_order: arduino_hal::spi::DataOrder::MostSignificantFirst,
            clock: arduino_hal::spi::SerialClockRate::OscfOver2,
            mode: embedded_hal::spi::MODE_0,
        },
    );

    #[cfg(feature = "debug")]
    let mut serial = arduino_hal::default_serial!(dp, pins, 57600);

    unsafe {
        avr_device::interrupt::enable();
    };

    init_async_adc(
        adc,
        &GLOBAL_ASYNC_ADC_STATE,
        [
            a4.into_channel(),
            a5.into_channel(),
            arduino_hal::adc::channel::ADC6.into_channel(),
            arduino_hal::adc::channel::ADC7.into_channel(),
        ],
    );

    let ui = UI::new(
        pins.d4.into_output(),
        pins.d5.into_output(),
        pins.d6.into_output(),
        pins.d7.into_output(),
    );

    // the jumpers choose the aux mode until one is set from the panel
    let jumper_aux = match (config_pin_1.is_high(), config_pin_2.is_high()) {
        (true, true) => AuxMode::EndOfRise,
        (false, true) => AuxMode::EndOfFall,
        (true, false) => AuxMode::NonZero,
        (false, false) => AuxMode::FollowGate,
    };

    let _ = config_pin_1.into_floating_input();
    let _ = config_pin_2.into_floating_input();

    let mut erase_eeprom = false;
    if btn_pin.is_low() {
        ui.update(0xF0);
        arduino_hal::delay_ms(100);
        if btn_pin.is_low() {
            erase_eeprom = true;
        }
    }

    let mut eeprom_data = Settings::DEFAULT.to_bytes();
    let mut eeprom = WearLevelledEepromWriter::<SAVED_SIZE>::init_and_advance(
        dp.EEPROM,
        &mut eeprom_data,
        erase_eeprom,
    );
    ui.update(0);

    while btn_pin.is_low() {
        unsafe { asm!("nop") };
    }

    // short click: next mode; hold: toggle time range; hold and turn a knob: hidden
    // settings
    let mut button = ButtonWithLongPress::<PB0, 32, 1000>::new(btn_pin);

    let sys_clock = SystemClock::init_system_clock(dp.TC0, &SYSTEM_CLOCK_STATE);

    let mut settings = Settings::from_bytes(&eeprom_data, EnvelopeMode::COUNT);
    // older saves (and invalid values) are rewritten in the current format
    let mut saved_bytes = eeprom_data;
    save_settings(&mut eeprom, &settings, &mut saved_bytes);
    set_long_time_range(settings.long_range);
    let mut config = settings.envelope_config();
    let mut envelope_state = EnvelopeState {
        mode: EnvelopeMode::from_index(settings.mode),
        time: 0,
        last_value: 0,
        artificial_gate: false,
    };

    let mut display = DisplayMode::Blink {
        pattern: ui_show_mode(&envelope_state.mode),
        until: UI_SHOW_ENVELOPE_MODE_MS,
    };

    ui.update(ui_show_mode(&envelope_state.mode));

    configure_timer(&dp.TC2);
    let mut dac = MCP4922::new(d10);
    dac.shutdown_channel(&mut spi, DacChannel::ChannelB);

    let mut aux_output_pin = pins.a3.into_output();
    let mut aux = AuxOutput::new();

    const LED_BLINK_INTERVAL_MS: u32 = 100;
    let mut led_blink_timer: u32 = 0;
    let mut led_blink_state: bool = false;

    let mut gate_was_high = false;

    let mut editor = Editor::new();
    let mut pickup = Pickup::new();
    let mut button_down_time: u32 = 0;
    let mut last_edited_knob: usize = 0;
    let mut last_edit_cv = [0u16; 4];
    // the LEDs show a hidden setting or a time range preview while the button is held
    let mut showing_edit = false;

    // knob values as the envelope sees them, updated by the UI work below
    let mut cv = interrupt::free(|cs| GLOBAL_ASYNC_ADC_STATE.get_inner(cs).get_all());

    loop {
        if !DAC_WRITE_QUEUED.atomic_read() {
            let gate_is_high = gate_pin.is_low();
            let gate = match (gate_was_high, gate_is_high) {
                (true, true) => GateState::High,
                (true, false) => GateState::Falling,
                (false, true) => GateState::Rising,
                (false, false) => GateState::Low,
            };
            gate_was_high = gate_is_high;
            let trigger = interrupt::free(|cs| {
                let mutex = QUEUED_TRIGGER.borrow(cs);
                let value = mutex.get();
                mutex.set(false);
                value
            });
            let input = Input { gate, trigger };
            #[cfg(feature = "profile")]
            set_profile_pin(true);
            let (value, did_change_phase) = update(&mut envelope_state, &input, &cv, &config);
            #[cfg(feature = "profile")]
            set_profile_pin(false);
            dac.write_keep_cs_pin_low(&mut spi, DacChannel::ChannelA, value, &Default::default());
            unsafe_access_mutex(|cs| DAC_WRITE_QUEUED.borrow(cs).set(true));

            if did_change_phase || aux.pulsing() || showing_edit {
                let aux_mode = settings.aux_mode.unwrap_or(jumper_aux);
                aux_output_pin
                    .set_state(aux.update(aux_flags(&envelope_state.mode), aux_mode).into())
                    .unwrap_infallible();
            }
            if did_change_phase && display == DisplayMode::ShowEnvelopeSegment && !showing_edit {
                ui.update(ui_show_stage(&envelope_state.mode));
            }
        }


        // The rest of the loop (knobs, button, LEDs) takes up to ~150 us. Only start
        // it if it will finish before the next sample is due, so it can never delay
        // computing one; it runs in whatever time the envelope math leaves over.
        if DAC_WRITE_QUEUED.atomic_read() && dp.TC2.tcnt2.read().bits() > SAMPLE_TICKS - UI_MAX_TICKS
        {
            continue;
        }

        #[cfg(feature = "profile")]
        set_ui_profile_pin(true);
        let raw_cv = interrupt::free(|cs| GLOBAL_ASYNC_ADC_STATE.get_inner(cs).get_all());
        let current_time = sys_clock.millis_exact();

        let mut new_blink = None;
        match button.sample(current_time) {
            LongPressButtonState::ButtonJustDown => {
                editor.press();
                button_down_time = current_time;
            }
            state @ (LongPressButtonState::ButtonHeldDownShort
            | LongPressButtonState::ButtonHeldDownLong
            | LongPressButtonState::ButtonJustClickedLong) => {
                let held_ms = current_time.wrapping_sub(button_down_time);
                if let Some(knob) = editor.hold(held_ms, &raw_cv) {
                    // only when the knob moved, to keep this pass short
                    if knob != last_edited_knob || raw_cv != last_edit_cv {
                        settings.set_from_knob(knob, &raw_cv);
                        config = settings.envelope_config();
                        ui.update(settings.leds(knob, jumper_aux));
                        last_edited_knob = knob;
                        last_edit_cv = raw_cv;
                    }
                    showing_edit = true;
                } else if state == LongPressButtonState::ButtonJustClickedLong {
                    // releasing now will toggle the range; preview the new one
                    ui.update(ui_show_time_range(!settings.long_range));
                    showing_edit = true;
                }
            }
            state @ (LongPressButtonState::ButtonJustClickedShort
            | LongPressButtonState::ButtonJustReleasedLong) => {
                showing_edit = false;
                new_blink = Some(if editor.release(&raw_cv, &mut pickup) {
                    settings.leds(last_edited_knob, jumper_aux)
                } else if state == LongPressButtonState::ButtonJustClickedShort {
                    envelope_state = envelope_state.cycle_mode();
                    settings.mode = envelope_state.mode.index();
                    ui_show_mode(&envelope_state.mode)
                } else {
                    settings.long_range = !settings.long_range;
                    set_long_time_range(settings.long_range);
                    ui_show_time_range(settings.long_range)
                });
                save_settings(&mut eeprom, &settings, &mut saved_bytes);
            }
            LongPressButtonState::ButtonIsUp => {}
        }

        if let Some(pattern) = new_blink {
            display = DisplayMode::Blink {
                pattern,
                until: current_time.wrapping_add(UI_SHOW_ENVELOPE_MODE_MS),
            };
            led_blink_timer = current_time.wrapping_add(LED_BLINK_INTERVAL_MS);
            led_blink_state = true;
            ui.update(pattern);
        }

        if let (DisplayMode::Blink { pattern, until }, false) = (display, showing_edit) {
            // wrapping comparisons so the ~49 day millis rollover is harmless
            if (current_time.wrapping_sub(until) as i32) > 0 {
                display = DisplayMode::ShowEnvelopeSegment;
                ui.update(ui_show_stage(&envelope_state.mode));
            } else if (current_time.wrapping_sub(led_blink_timer) as i32) > 0 {
                led_blink_timer = current_time.wrapping_add(LED_BLINK_INTERVAL_MS);
                led_blink_state = !led_blink_state;
                ui.update(if led_blink_state { pattern } else { 0 });
            }
        }

        // knobs being used for (or just used for) a hidden setting keep their old value
        cv = raw_cv;
        editor.freeze(&mut cv);
        pickup.apply(&mut cv);
        #[cfg(feature = "profile")]
        set_ui_profile_pin(false);

        #[cfg(feature = "debug")]
        {
            use ufmt::uwrite;
            let num_skipped = DEBUG_SKIPPED_WRITE_COUNT.atomic_read();
            if num_skipped != 0 {
                unsafe_access_mutex(|cs| DEBUG_SKIPPED_WRITE_COUNT.borrow(cs).set(0));
                for _ in 0..num_skipped {
                    uwrite!(&mut serial, ".").unwrap_infallible();
                }
            }
        }
    }
}

/// Writes the bytes of `settings` that differ from what was last saved
fn save_settings(
    eeprom: &mut WearLevelledEepromWriter<SAVED_SIZE>,
    settings: &Settings,
    saved: &mut [u8; SAVED_SIZE],
) {
    let bytes = settings.to_bytes();
    for i in 0..SAVED_SIZE {
        if bytes[i] != saved[i] {
            eeprom.update_byte(i as u16, bytes[i]);
            saved[i] = bytes[i];
        }
    }
}

/// A0 is high during the UI part of the main loop (with the `profile` feature)
#[cfg(feature = "profile")]
fn set_ui_profile_pin(high: bool) {
    let dp = unsafe { arduino_hal::Peripherals::steal() };
    dp.PORTC.ddrc.modify(|r, w| unsafe { w.bits(r.bits() | 0b1) });
    dp.PORTC.portc.modify(|r, w| unsafe { w.bits(if high { r.bits() | 0b1 } else { r.bits() & !0b1 }) });
}

/// D9 is high while the envelope math runs (with the `profile` feature)
#[cfg(feature = "profile")]
fn set_profile_pin(high: bool) {
    let dp = unsafe { arduino_hal::Peripherals::steal() };
    dp.PORTB.ddrb.modify(|r, w| unsafe { w.bits(r.bits() | 0b10) });
    dp.PORTB.portb.modify(|r, w| unsafe { w.bits(if high { r.bits() | 0b10 } else { r.bits() & !0b10 }) });
}

/// TIMER2 counts 4 us ticks from one sample to the next (0..SAMPLE_TICKS)
const SAMPLE_TICKS: u8 = 120;
/// Time the UI part of the main loop is given, in TIMER2 ticks. Measured in simavr: it
/// usually takes ~110 us, up to ~280 us on the rare pass that applies a setting edit
const UI_MAX_TICKS: u8 = 50;

fn configure_timer(tc2: &arduino_hal::pac::TC2) {
    // reset timer counter at TOP set by OCRA
    tc2.tccr2a.write(|w| w.wgm2().ctc());
    // set timer frequency to 2083.3Hz = one sample every 480us, which the envelope
    // timing in envelope/shared.rs relies on
    // (16MHz clock speed / 64 prescale factor / 120 counts; the counter goes 0..=OCR2A)
    tc2.tccr2b.write(|w| w.cs2().prescale_64());
    tc2.ocr2a.write(|w| w.bits(SAMPLE_TICKS - 1));

    // enable interrupt on match to compare register A
    tc2.timsk2.write(|w| w.ocie2a().set_bit());
}

struct UI {
    _d4: Pin<Output, port::PD4>,
    _d5: Pin<Output, port::PD5>,
    _d6: Pin<Output, port::PD6>,
    _d7: Pin<Output, port::PD7>,
}

impl UI {
    fn new(
        d4: Pin<Output, port::PD4>,
        d5: Pin<Output, port::PD5>,
        d6: Pin<Output, port::PD6>,
        d7: Pin<Output, port::PD7>,
    ) -> Self {
        UI {
            _d4: d4,
            _d5: d5,
            _d6: d6,
            _d7: d7,
        }
    }

    fn update(&self, ui_state: u8) {
        debug_assert!(ui_state & 0xf == 0);
        unsafe {
            let dp = arduino_hal::Peripherals::steal();
            dp.PORTD
                .portd
                .modify(|r, w| w.bits(r.bits() & 0xf | ui_state))
        }
    }
}
