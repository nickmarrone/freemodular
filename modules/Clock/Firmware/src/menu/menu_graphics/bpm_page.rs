use avr_progmem::progmem;
use embedded_graphics::pixelcolor::BinaryColor;

use crate::{
    clock::ClockConfig,
    display_buffer::{Justify, MiniBuffer, TextColor},
    font::{PRO_FONT_22, PRO_FONT_29_NUMERIC},
    menu::{menu_state::EditingState, MenuUpdate},
    render_numbers::{u16_to_str_b10, CHAR_PERIOD},
};

progmem! {
    static progmem BPM_TEXT_IMG: [u8; 19] = *include_bytes!("../../../assets/bpm_text.bin");
    static progmem TAP_TEXT_IMG: [u8; 19] = *include_bytes!("../../../assets/tap_text.bin");
}

#[inline(never)]
pub fn render_bpm_page<DI, SIZE>(
    editing: EditingState,
    clock_state: &ClockConfig,
    menu_update: &MenuUpdate,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    if *menu_update == MenuUpdate::SwitchScreens {
        let _ = display.clear();
    }

    // whole BPM in the big font
    {
        let mut buffer: [u8; 3] = [0u8; 3];
        let text = u16_to_str_b10(&mut buffer, clock_state.bpm10 / 10);
        let mut mini_buffer = MiniBuffer::<64, 40>::new();
        let inverted = editing == EditingState::Editing || editing == EditingState::Tap;
        if inverted {
            mini_buffer.fast_fill(0, 4, 64, 32, BinaryColor::On);
        }
        mini_buffer.fast_draw_ascii_text(
            Justify::Center(32),
            Justify::Center(20),
            text,
            &PRO_FONT_29_NUMERIC,
            if inverted {
                &TextColor::BinaryOffTransparent
            } else {
                &TextColor::BinaryOn
            },
        );
        let _ = mini_buffer.blit(display, 32, 8);
    }

    // tenths of a BPM in the small font
    {
        let text = [CHAR_PERIOD, b'0' + (clock_state.bpm10 % 10) as u8];
        let mut mini_buffer = MiniBuffer::<24, 24>::new();
        let fine = editing == EditingState::EditingFine;
        if fine {
            let _ = mini_buffer.clear(BinaryColor::On);
        }
        mini_buffer.fast_draw_ascii_text(
            Justify::Start(0),
            Justify::Start(2),
            &text,
            &PRO_FONT_22,
            if fine {
                &TextColor::BinaryOffTransparent
            } else {
                &TextColor::BinaryOn
            },
        );
        let _ = mini_buffer.blit(display, 96, 16);
    }

    // Because the label fits perfectly in the native 8px pages and there is no
    // compositing, there is no need to use a mini buffer here
    let img = if editing == EditingState::Tap {
        TAP_TEXT_IMG.load()
    } else {
        BPM_TEXT_IMG.load()
    };
    let (x, y, w, h) = (54, 48, 19, 8);
    let _ = display.set_draw_area((x, y), (x + w, y + h));
    let _ = display.draw(&img);
}
