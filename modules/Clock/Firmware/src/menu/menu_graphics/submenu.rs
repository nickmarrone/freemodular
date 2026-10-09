use avr_progmem::progmem;
use embedded_graphics::pixelcolor::BinaryColor;

use crate::{
    clock::ClockConfig,
    display_buffer::{Justify, MiniBuffer, TextColor},
    font::PRO_FONT_22,
    menu::{
        menu_state::{EditingState, MenuItem, MenuPage, MenuState},
        MenuUpdate,
    },
    render_numbers::{
        i8_to_str_b10, tempo_to_str, u16_to_str_b10, word, CHAR_PERCENT, WORD_INVT, WORD_OFF,
        WORD_STOP_NOW, WORD_TRIG,
    },
};

/// Renders the scrolling list pages: the per-channel detail page and the global page.
/// Two rows are visible at a time.
#[inline(never)]
pub fn render_list_page<DI, SIZE>(
    menu_state: &MenuState,
    config: &ClockConfig,
    menu_update: &MenuUpdate,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    let (cursor, scroll, channel) = match menu_state.page {
        MenuPage::SubMenu {
            cursor,
            scroll,
            channel,
        } => (cursor, scroll, channel),
        MenuPage::Global { cursor, scroll } => (cursor, scroll, 0),
        _ => return,
    };
    let page = &menu_state.page;
    match menu_update {
        MenuUpdate::UpdateValueAtCursor | MenuUpdate::ToggleEditingAtCursor => {
            draw_item_value(
                row_y(cursor - scroll),
                true,
                menu_state.editing,
                page.list_item(cursor, config),
                config,
                channel,
                menu_state.slot,
                display,
            );
        }
        MenuUpdate::MoveCursorFrom(_) | MenuUpdate::Scroll | MenuUpdate::SwitchScreens => {
            let len = page.list_len(config);
            draw_arrows(true, scroll > 0, display);
            draw_arrows(false, scroll + 2 < len, display);
            for i in scroll..(scroll + 2).min(len) {
                let selected = cursor == i;
                let item = page.list_item(i, config);
                let y = row_y(i - scroll);
                draw_item_label(y, selected, item, display);
                draw_item_value(
                    y,
                    selected,
                    if selected {
                        menu_state.editing
                    } else {
                        EditingState::Navigating
                    },
                    item,
                    config,
                    channel,
                    menu_state.slot,
                    display,
                );
            }
        }
        MenuUpdate::NoUpdate | MenuUpdate::ScreenSaverStep(_) => {}
    }
}

fn row_y(row: u8) -> u8 {
    row * 24 + 8
}

progmem! {
    static progmem TRIANGLE_DOWN: [u8; 11] = [
        0b00000100,
        0b00001100,
        0b00011100,
        0b00111100,
        0b01111100,
        0b11111100,
        0b01111100,
        0b00111100,
        0b00011100,
        0b00001100,
        0b00000100,
    ];
    static progmem TRIANGLE_UP: [u8; 11] = [
        0b00100000,
        0b00110000,
        0b00111000,
        0b00111100,
        0b00111110,
        0b00111111,
        0b00111110,
        0b00111100,
        0b00111000,
        0b00110000,
        0b00100000,
    ];
    static progmem RETURN_ARROW: [u8; 57] = *include_bytes!("../../../assets/back_arrow.bin");
    static progmem SLASH_64: [u8; 36] = *include_bytes!("../../../assets/div_64.bin");
    /// Labels for each MenuItem, in order
    static progmem LABELS: [[u8; 6]; 13] = [
        *b"Tempo\0", *b"Tuplet", *b"PulseW", *b"Phase\0", *b"Swing\0", *b"Prob\0\0",
        *b"Steps\0", *b"Fill\0\0", *b"Rotate", *b"Exit\0\0", *b"Stop\0\0", *b"Load\0\0",
        *b"Save\0\0",
    ];
}

#[inline(never)]
fn draw_arrows<DI, SIZE>(
    top: bool,
    draw_arrows: bool,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    let mut buffer = MiniBuffer::<128, 8>::new();
    let y = if top { 0 } else { 64 - 8 };

    if draw_arrows {
        let img_progmem = if top { TRIANGLE_UP } else { TRIANGLE_DOWN };
        let img = &img_progmem.load();

        let img_width: u8 = img.len() as u8;
        let spacing: u8 = 30;
        let center: u8 = 128 / 2;

        let offsets = [
            center - img_width / 2,
            center - spacing - img_width / 2,
            center + spacing - img_width / 2,
        ];
        for offset in offsets {
            buffer.fast_draw_image(offset as usize, 0, img_width, 8, img, &TextColor::BinaryOn);
        }
    }

    let _ = buffer.blit(display, 0, y);
}

enum Symbol {
    None,
    Percent,
    SixtyFourths,
}

#[inline(never)]
fn draw_item_value<DI, SIZE>(
    y_offset: u8,
    selected: bool,
    editing: EditingState,
    item: MenuItem,
    config: &ClockConfig,
    channel_idx: u8,
    slot: u8,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    let channel = &config.channels[channel_idx as usize];
    let mut buffer = MiniBuffer::<54, 24>::new();
    let editing = editing.is_editing();
    let text_color = if selected && !editing {
        let _ = buffer.clear(BinaryColor::On);
        TextColor::BinaryOff
    } else {
        TextColor::BinaryOn
    };
    let mut text_buffer = [0u8; 5];
    let mut symbol = Symbol::None;
    let text: &[u8] = match item {
        MenuItem::Division => tempo_to_str(&mut text_buffer, channel.division, channel.tuplet),
        MenuItem::Tuplet => word(&mut text_buffer, WORD_OFF + channel.tuplet),
        MenuItem::PulseWidth => match channel.pulse_width {
            0 => word(&mut text_buffer, WORD_TRIG),
            100 => word(&mut text_buffer, WORD_INVT),
            pulse_width => {
                symbol = Symbol::Percent;
                u16_to_str_b10(&mut text_buffer, pulse_width as u16)
            }
        },
        MenuItem::PhaseShift => {
            symbol = Symbol::SixtyFourths;
            i8_to_str_b10(&mut text_buffer, channel.phase_shift)
        }
        MenuItem::Swing => {
            symbol = Symbol::SixtyFourths;
            u16_to_str_b10(&mut text_buffer, channel.swing as u16)
        }
        MenuItem::Probability => {
            symbol = Symbol::Percent;
            u16_to_str_b10(&mut text_buffer, channel.probability as u16)
        }
        MenuItem::EuclidSteps if channel.euclid_steps == 0 => word(&mut text_buffer, WORD_OFF),
        MenuItem::EuclidSteps => u16_to_str_b10(&mut text_buffer, channel.euclid_steps as u16),
        MenuItem::EuclidFill => u16_to_str_b10(&mut text_buffer, channel.euclid_fill as u16),
        MenuItem::EuclidRotate => u16_to_str_b10(&mut text_buffer, channel.euclid_rotate as u16),
        MenuItem::StopMode => word(&mut text_buffer, WORD_STOP_NOW + config.stop_mode),
        MenuItem::Load | MenuItem::Save => u16_to_str_b10(&mut text_buffer, slot as u16 + 1),
        MenuItem::Exit => &[],
    };
    let mut align_to: u8 = 52;
    match symbol {
        Symbol::None => {}
        Symbol::Percent => {
            align_to -= 12;
            buffer.fast_draw_ascii_text(
                Justify::Start(align_to as usize),
                Justify::Start(1),
                &[CHAR_PERCENT],
                &PRO_FONT_22,
                &text_color,
            );
        }
        Symbol::SixtyFourths => {
            align_to -= 12;
            buffer.fast_draw_image(align_to as usize, 1, 12, 24, &SLASH_64.load(), &text_color);
        }
    }
    buffer.fast_draw_ascii_text(
        Justify::End(align_to as usize),
        Justify::Start(1),
        text,
        &PRO_FONT_22,
        &text_color,
    );
    if editing {
        buffer.fast_rect(0, 0, 54, 24, BinaryColor::On, 2);
    }
    let _ = buffer.blit(display, 74, y_offset);
}

#[inline(never)]
fn draw_item_label<DI, SIZE>(
    y_offset: u8,
    invert: bool,
    item: MenuItem,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    let mut buffer = MiniBuffer::<74, 24>::new();

    if invert {
        let _ = buffer.clear(BinaryColor::On);
    }

    let text_color = match invert {
        true => &TextColor::BinaryOff,
        false => &TextColor::BinaryOn,
    };

    let mut x = 2;
    if item == MenuItem::Exit {
        let img = RETURN_ARROW.load();
        buffer.fast_draw_image(2, 0, 19, 24, &img, text_color);
        x = 26;
    }
    let label = LABELS.load_at(item as usize);
    let len = label.iter().position(|c| *c == 0).unwrap_or(label.len());
    buffer.fast_draw_ascii_text(
        Justify::Start(x),
        Justify::Start(1),
        &label[..len],
        &PRO_FONT_22,
        text_color,
    );
    let _ = buffer.blit(display, 0, y_offset);
}
