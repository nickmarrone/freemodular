mod bpm_page;
mod main_page;
mod screen_saver;
mod submenu;

use crate::{
    clock::ClockConfig,
    display_buffer::{Justify, MiniBuffer, TextColor},
    font::PRO_FONT_22,
};

use self::{
    bpm_page::render_bpm_page, main_page::render_main_page, screen_saver::render_screensaver,
    submenu::render_list_page,
};

use super::{
    menu_state::{MenuPage, MenuUpdate},
    MenuOrScreenSaverState,
};

#[inline(never)]
pub fn render_menu<DI, SIZE>(
    menu_state: &MenuOrScreenSaverState,
    clock_state: &ClockConfig,
    menu_update: &MenuUpdate,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    match menu_state {
        MenuOrScreenSaverState::ScreenSaver(ss_state) => {
            render_screensaver(ss_state, menu_update, display);
        }
        MenuOrScreenSaverState::Menu(menu_state) => match menu_state.page {
            MenuPage::Bpm => render_bpm_page(menu_state.editing, clock_state, menu_update, display),
            MenuPage::Main { cursor } => render_main_page(
                cursor,
                menu_state.editing,
                clock_state,
                menu_update,
                display,
            ),
            MenuPage::SubMenu { .. } | MenuPage::Global { .. } => {
                render_list_page(menu_state, clock_state, menu_update, display)
            }
            MenuPage::ConfirmReset => render_confirm_reset(menu_update, display),
        },
    }
}

#[inline(never)]
fn render_confirm_reset<DI, SIZE>(
    menu_update: &MenuUpdate,
    display: &mut ssd1306::Ssd1306<DI, SIZE, ssd1306::mode::BasicMode>,
) where
    DI: display_interface::WriteOnlyDataCommand,
    SIZE: ssd1306::size::DisplaySize,
{
    if *menu_update != MenuUpdate::SwitchScreens {
        return;
    }
    let _ = display.clear();
    let mut buffer = MiniBuffer::<72, 24>::new();
    buffer.fast_draw_ascii_text(
        Justify::Start(0),
        Justify::Start(1),
        b"Reset?",
        &PRO_FONT_22,
        &TextColor::BinaryOn,
    );
    let _ = buffer.blit(display, 28, 16);
}
