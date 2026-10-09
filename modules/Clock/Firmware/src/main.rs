#![allow(incomplete_features)]
#![no_std]
#![no_main]
#![feature(generic_const_exprs)]
#![feature(int_roundings)]
#![feature(abi_avr_interrupt)]
#![feature(inline_const_pat)]
#![feature(const_trait_impl)]
#![feature(adt_const_params)]

mod clock;
mod display_buffer;
mod eeprom;
mod font;
mod menu;
mod random;
mod render_numbers;

use arduino_hal::hal::port::{PC3, PC4};
use avr_device::interrupt;
use clock::ClockConfig;
use core::panic::PanicInfo;
use eeprom::PersistanceManager;
use fm_lib::button_debouncer::{ButtonWithLongPress, LongPressButtonState};
use fm_lib::debug_unwrap::DebugUnwrap;
use fm_lib::rotary_encoder::RotaryEncoderHandler;
use menu::{
    render_menu, update_menu, ConfigChange, MenuOrScreenSaverState, MenuPage, MenuState,
    MenuUpdate,
};
use ssd1306::{prelude::*, Ssd1306};

#[inline(never)]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    interrupt::disable();

    let dp = unsafe { arduino_hal::Peripherals::steal() };
    dp.PORTD.portd.write(|w| unsafe { w.bits(0) });
    dp.PORTB.ddrb.write(|w| w.pb5().set_bit());
    loop {
        // writing to PINx toggles the pin
        dp.PORTB.pinb.write(|w| w.pb5().set_bit());
        arduino_hal::delay_ms(150);
    }
}

static ROTARY_ENCODER: RotaryEncoderHandler = RotaryEncoderHandler::new();

/**
 Pin-change interrupt handler for port B (pins d8-d13)
*/
#[avr_device::interrupt(atmega328p)]
#[allow(non_snake_case)]
fn PCINT0() {
    let dp = unsafe { arduino_hal::Peripherals::steal() };
    let port = dp.PORTB.pinb.read();
    let a = port.pb0().bit_is_set();
    let b = port.pb1().bit_is_set();
    ROTARY_ENCODER.update(a, b);
}

#[arduino_hal::entry]
fn main() -> ! {
    let dp = arduino_hal::Peripherals::take().assert_ok();

    let mut clock_config = ClockConfig::new();
    let mut persistance_manager = PersistanceManager::new(&mut clock_config);

    // set pins d0-d7 as output
    dp.PORTD.ddrd.write(|w| unsafe { w.bits(0xff) });

    // Enable pin-change interrupts on pins d8 and d9
    dp.PORTB.ddrb.write(|w| w.pb0().bit(false).pb1().bit(false));
    dp.PORTB.portb.write(|w| w.pb0().bit(true).pb1().bit(true));
    dp.EXINT.pcifr.reset();
    dp.EXINT.pcmsk0.write(|w| w.pcint().bits(0b00000011));
    dp.EXINT.pcicr.write(|w| w.pcie().bits(0b001));

    // start the clock engine; outputs are driven from the TIMER1 interrupt
    clock::init_timer(dp.TC1);
    clock::start(&clock_config);

    // turn on interrupts
    unsafe {
        avr_device::interrupt::enable();
    };

    // setup display
    let pins = arduino_hal::pins!(dp);
    let mut display = {
        let (spi, _) = arduino_hal::spi::Spi::new(
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
        let interface = display_interface_spi::SPIInterface::new(
            spi,
            pins.a1.into_output(),
            pins.a2.into_output(),
        );

        let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0);
        let _ = display.reset(&mut pins.a0.into_output(), &mut arduino_hal::Delay::new());
        let _ = display.init_with_addr_mode(ssd1306::command::AddrMode::Vertical);
        let _ = display.clear();
        display
    };

    // set up app state
    let mut encoder_button = ButtonWithLongPress::<PC4, 32, 500>::new(pins.a4.into_pull_up_input());
    let mut pause_button = ButtonWithLongPress::<PC3, 32, 2000>::new(pins.a3.into_pull_up_input());
    let mut menu_state = MenuOrScreenSaverState::new(clock::millis());

    render_menu(
        &menu_state,
        &clock_config,
        &MenuUpdate::SwitchScreens,
        &mut display,
    );

    // Main loop. Only handles the UI; clock timing is entirely interrupt driven.
    loop {
        let current_time_ms = clock::millis();

        // Handle pause button
        match pause_button.sample(current_time_ms) {
            LongPressButtonState::ButtonJustDown => {
                if !clock::is_running() {
                    clock::start(&clock_config);
                } else if clock::stop_is_pending() {
                    clock::cancel_stop();
                } else {
                    clock::stop(clock_config.stop_mode);
                }
            }
            LongPressButtonState::ButtonJustClickedLong => {
                // ask for confirmation before erasing everything
                let mut state = MenuState::new(current_time_ms);
                state.page = MenuPage::ConfirmReset;
                menu_state = MenuOrScreenSaverState::Menu(state);
                render_menu(
                    &menu_state,
                    &clock_config,
                    &MenuUpdate::SwitchScreens,
                    &mut display,
                );
            }
            _ => {}
        }

        // Handle menu logic
        let (mut menu_update, change) = update_menu(
            &mut menu_state,
            &mut clock_config,
            &mut encoder_button,
            &ROTARY_ENCODER,
            current_time_ms,
            clock::take_beat_flag(),
            &mut persistance_manager,
        );

        match change {
            ConfigChange::None => {}
            ConfigChange::Params => {
                clock::apply_config(&clock_config, 0);
                persistance_manager.mark_dirty(current_time_ms);
            }
            ConfigChange::Realign(mask) => {
                clock::apply_config(&clock_config, mask);
                persistance_manager.mark_dirty(current_time_ms);
            }
            ConfigChange::FactoryReset => {
                clock_config = ClockConfig::new();
                persistance_manager.save(&clock_config);
                clock::start(&clock_config);
                menu_state = MenuOrScreenSaverState::new(current_time_ms);
                menu_update = MenuUpdate::SwitchScreens;
            }
        }
        persistance_manager.poll(&clock_config, current_time_ms);

        // Only re-render the part of the screen that needs to be updated, if any
        if menu_update != MenuUpdate::NoUpdate {
            render_menu(&menu_state, &clock_config, &menu_update, &mut display);
        }
    }
}
