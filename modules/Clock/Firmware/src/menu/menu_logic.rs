use arduino_hal::port::PinOps;
use fm_lib::button_debouncer::{ButtonWithLongPress, LongPressButtonState};
use fm_lib::rotary_encoder::RotaryEncoderHandler;

use crate::clock::{
    is_special_division, ClockConfig, MAX_BPM10, MAX_EUCLID_STEPS, MIN_BPM10, STOP_BAR,
    TUPLET_DOTTED,
};
use crate::eeprom::{PersistanceManager, NUM_PRESETS};

use super::{
    menu_state::*,
    utils::{single_step_clock_division, step_clock_division},
};

/// What the menu changed in the clock config, so the engine can be updated
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ConfigChange {
    None,
    /// Only values that don't move channels in time (tempo, pulse width, ...)
    Params,
    /// Bit mask of channels whose rate/phase/pattern changed and must be re-aligned
    Realign(u8),
    FactoryReset,
}

const SCREENSAVER_TIMEOUT_MS: u32 = 5000;
const TAP_TIMEOUT_MS: u32 = 3000;
const MAX_TAP_INTERVAL_MS: u32 = 2000;
/// Marks that tap mode was just entered, so the first tap only starts the sequence
const NO_TAPS_YET: u8 = 0xff;
/// Encoder detents closer together than this are accelerated
const ACCELERATION_MS: u32 = 40;

#[inline(never)]
pub fn update_menu<BtnPin, const BTN_DEBOUNCE: u32, const BTN_LONG_PRESS: u32>(
    menu_or_ss_state: &mut MenuOrScreenSaverState,
    config: &mut ClockConfig,
    button: &mut ButtonWithLongPress<BtnPin, BTN_DEBOUNCE, BTN_LONG_PRESS>,
    rotary_encoder: &RotaryEncoderHandler,
    current_time_ms: u32,
    did_rollover: bool,
    persistance_manager: &mut PersistanceManager,
) -> (MenuUpdate, ConfigChange)
where
    BtnPin: PinOps,
{
    let no_change = |update| (update, ConfigChange::None);
    match menu_or_ss_state {
        MenuOrScreenSaverState::ScreenSaver(ref mut ss_state) => {
            let button_state = button.sample(current_time_ms);
            let rotary_encoder_delta = rotary_encoder.sample_and_reset();
            if button_state == LongPressButtonState::ButtonJustDown || rotary_encoder_delta != 0 {
                *menu_or_ss_state = MenuOrScreenSaverState::new(current_time_ms);
                return no_change(MenuUpdate::SwitchScreens);
            }

            if did_rollover {
                return no_change(screensaver_step(ss_state));
            }

            no_change(MenuUpdate::NoUpdate)
        }
        MenuOrScreenSaverState::Menu(ref mut menu_state) => {
            let button_state = button.sample(current_time_ms);
            match button_state {
                LongPressButtonState::ButtonJustDown => {
                    menu_state.last_input_time_ms = current_time_ms;
                    return handle_short_press(
                        menu_state,
                        config,
                        current_time_ms,
                        persistance_manager,
                    );
                }
                LongPressButtonState::ButtonJustClickedLong => {
                    menu_state.last_input_time_ms = current_time_ms;
                    return no_change(handle_long_press(menu_state, current_time_ms));
                }
                LongPressButtonState::ButtonIsUp => {}
                _ => {
                    menu_state.last_input_time_ms = current_time_ms;
                }
            }

            let mut rotary_encoder_delta = rotary_encoder.sample_and_reset();
            if rotary_encoder_delta != 0 {
                let fast = current_time_ms.wrapping_sub(menu_state.last_turn_time_ms)
                    < ACCELERATION_MS;
                menu_state.last_turn_time_ms = current_time_ms;
                menu_state.last_input_time_ms = current_time_ms;
                if fast {
                    rotary_encoder_delta = rotary_encoder_delta.saturating_mul(4);
                }
                return handle_rotary_knob_change(menu_state, config, rotary_encoder_delta, fast);
            }

            if menu_state.editing == EditingState::Tap
                && current_time_ms.wrapping_sub(menu_state.tap.last_tap_ms) > TAP_TIMEOUT_MS
            {
                menu_state.editing = EditingState::Navigating;
                return no_change(MenuUpdate::ToggleEditingAtCursor);
            }

            if current_time_ms.wrapping_sub(menu_state.last_input_time_ms) > SCREENSAVER_TIMEOUT_MS
            {
                *menu_or_ss_state =
                    MenuOrScreenSaverState::ScreenSaver(ScreenSaverState::new(current_time_ms));
                return no_change(MenuUpdate::SwitchScreens);
            }

            no_change(MenuUpdate::NoUpdate)
        }
    }
}

/// Fill in one random block of the screensaver
fn screensaver_step(ss_state: &mut ScreenSaverState) -> MenuUpdate {
    const COL_MAX: u8 = 7;

    let mut starting_col = ss_state.rng.next() % 16;
    let mut one_col_has_space = false;
    for i in 0..16 {
        let idx = (starting_col + i) % 16;
        if ss_state.y_offsets[idx as usize] < COL_MAX {
            starting_col = idx;
            one_col_has_space = true;
            break;
        }
    }

    let col = if !one_col_has_space {
        ss_state.y_offsets = [0; 16];
        ss_state.color = !ss_state.color;
        starting_col
    } else {
        let mut num_steps = (ss_state.rng.next() % 16) + 1;
        let mut step = 0;
        let mut idx: u8 = 0;
        while num_steps > 0 {
            idx = (starting_col + step) % 16;
            step += 1;
            if ss_state.y_offsets[idx as usize] < COL_MAX {
                num_steps -= 1;
            }
        }
        idx
    };
    ss_state.y_offsets[col as usize] += 1;
    MenuUpdate::ScreenSaverStep(col)
}

fn handle_long_press(menu_state: &mut MenuState, current_time_ms: u32) -> MenuUpdate {
    match menu_state.page {
        MenuPage::Bpm => {
            menu_state.editing = EditingState::Tap;
            menu_state.tap.num_intervals = NO_TAPS_YET;
            menu_state.tap.last_tap_ms = current_time_ms;
            return MenuUpdate::ToggleEditingAtCursor;
        }
        MenuPage::Main { cursor } => {
            menu_state.page = MenuPage::SubMenu {
                channel: cursor,
                cursor: 0,
                scroll: 0,
            };
        }
        MenuPage::SubMenu { channel, .. } => {
            menu_state.page = MenuPage::Main { cursor: channel };
        }
        MenuPage::Global { .. } | MenuPage::ConfirmReset => return MenuUpdate::NoUpdate,
    }
    menu_state.editing = EditingState::Navigating;
    MenuUpdate::SwitchScreens
}

fn handle_tap(menu_state: &mut MenuState, config: &mut ClockConfig, now: u32) -> ConfigChange {
    let tap = &mut menu_state.tap;
    let interval = now.wrapping_sub(tap.last_tap_ms);
    tap.last_tap_ms = now;
    if interval > MAX_TAP_INTERVAL_MS || tap.num_intervals == NO_TAPS_YET {
        // first tap of a new sequence
        tap.num_intervals = 0;
        return ConfigChange::None;
    }
    tap.intervals[tap.next as usize] = interval as u16;
    tap.next = (tap.next + 1) % tap.intervals.len() as u8;
    tap.num_intervals = (tap.num_intervals + 1).min(tap.intervals.len() as u8);
    let sum: u32 = tap.intervals[..tap.num_intervals as usize]
        .iter()
        .map(|x| *x as u32)
        .sum();
    // bpm10 = 60000ms * 10 / average interval
    config.bpm10 = ((600_000 * tap.num_intervals as u32 / sum.max(1)) as u16)
        .clamp(MIN_BPM10, MAX_BPM10);
    ConfigChange::Params
}

fn handle_short_press(
    menu_state: &mut MenuState,
    config: &mut ClockConfig,
    current_time_ms: u32,
    persistance_manager: &mut PersistanceManager,
) -> (MenuUpdate, ConfigChange) {
    let mut change = ConfigChange::None;
    let update = match menu_state.page {
        MenuPage::Bpm => {
            menu_state.editing = match menu_state.editing {
                EditingState::Navigating => EditingState::Editing,
                EditingState::Editing => EditingState::EditingFine,
                EditingState::EditingFine => EditingState::Navigating,
                EditingState::Tap => {
                    change = handle_tap(menu_state, config, current_time_ms);
                    return (MenuUpdate::UpdateValueAtCursor, change);
                }
            };
            MenuUpdate::ToggleEditingAtCursor
        }
        MenuPage::Main { .. } => {
            menu_state.editing = menu_state.editing.toggle();
            MenuUpdate::ToggleEditingAtCursor
        }
        MenuPage::ConfirmReset => return (MenuUpdate::NoUpdate, ConfigChange::FactoryReset),
        MenuPage::Global { cursor, .. } | MenuPage::SubMenu { cursor, .. } => {
            match menu_state.page.list_item(cursor, config) {
                MenuItem::Exit => {
                    if let MenuPage::SubMenu { channel, .. } = menu_state.page {
                        menu_state.page = MenuPage::Main { cursor: channel };
                    }
                    return (MenuUpdate::SwitchScreens, change);
                }
                item @ (MenuItem::Load | MenuItem::Save) if menu_state.editing.is_editing() => {
                    // second click commits the selected preset slot
                    if item == MenuItem::Save {
                        persistance_manager.save_preset(menu_state.slot, config);
                    } else if persistance_manager.load_preset(menu_state.slot, config) {
                        change = ConfigChange::Realign(0xff);
                    }
                }
                _ => {}
            }
            menu_state.editing = menu_state.editing.toggle();
            MenuUpdate::ToggleEditingAtCursor
        }
    };
    (update, change)
}

fn step_u8(value: u8, delta: i8, max: u8) -> u8 {
    value.saturating_add_signed(delta).min(max)
}

fn handle_rotary_knob_change(
    menu_state: &mut MenuState,
    config: &mut ClockConfig,
    delta: i8,
    fast: bool,
) -> (MenuUpdate, ConfigChange) {
    match menu_state.page {
        MenuPage::ConfirmReset => {
            menu_state.page = MenuPage::Bpm;
            (MenuUpdate::SwitchScreens, ConfigChange::None)
        }
        MenuPage::Bpm => match menu_state.editing {
            EditingState::Editing | EditingState::EditingFine => {
                let step: i16 = if menu_state.editing == EditingState::Editing {
                    10
                } else {
                    1
                };
                config.bpm10 = (config.bpm10 as i16 + delta as i16 * step)
                    .clamp(MIN_BPM10 as i16, MAX_BPM10 as i16) as u16;
                (MenuUpdate::UpdateValueAtCursor, ConfigChange::Params)
            }
            EditingState::Tap => {
                menu_state.editing = EditingState::Navigating;
                (MenuUpdate::ToggleEditingAtCursor, ConfigChange::None)
            }
            EditingState::Navigating => {
                menu_state.page = if delta > 0 {
                    MenuPage::Main {
                        cursor: (delta as u8 - 1).min(7),
                    }
                } else {
                    MenuPage::Global {
                        cursor: NUM_LAST_GLOBAL,
                        scroll: NUM_LAST_GLOBAL - 1,
                    }
                };
                (MenuUpdate::SwitchScreens, ConfigChange::None)
            }
        },
        MenuPage::Main { ref mut cursor } => match menu_state.editing {
            EditingState::Editing => {
                let channel = &mut config.channels[*cursor as usize];
                // fast-edit mode always moves one power of two at a time
                channel.division = step_clock_division(channel.division, delta.signum());
                (
                    MenuUpdate::UpdateValueAtCursor,
                    ConfigChange::Realign(1 << *cursor),
                )
            }
            _ => {
                let old_cursor = *cursor;
                let new_cursor = (old_cursor as i8) + delta;

                if new_cursor < 0 {
                    menu_state.page = MenuPage::Bpm;
                    (MenuUpdate::SwitchScreens, ConfigChange::None)
                } else {
                    *cursor = (new_cursor as u8).min(7);
                    (MenuUpdate::MoveCursorFrom(old_cursor), ConfigChange::None)
                }
            }
        },
        MenuPage::Global { cursor, scroll } | MenuPage::SubMenu { cursor, scroll, .. } => {
            let channel_idx = match menu_state.page {
                MenuPage::SubMenu { channel, .. } => channel,
                _ => 0,
            };
            if menu_state.editing.is_editing() {
                let item = menu_state.page.list_item(cursor, config);
                return edit_item(item, menu_state, config, channel_idx, delta, fast);
            }
            let len = menu_state.page.list_len(config);
            let new_cursor = cursor as i8 + delta;
            if new_cursor >= len as i8 && matches!(menu_state.page, MenuPage::Global { .. }) {
                // the global page sits above the BPM page
                menu_state.page = MenuPage::Bpm;
                return (MenuUpdate::SwitchScreens, ConfigChange::None);
            }
            let new_cursor = new_cursor.clamp(0, len as i8 - 1) as u8;
            let mut new_scroll = scroll;
            let update = if new_cursor == cursor {
                MenuUpdate::NoUpdate
            } else if new_cursor < scroll {
                new_scroll = new_cursor;
                MenuUpdate::Scroll
            } else if new_cursor > scroll + 1 {
                new_scroll = new_cursor - 1;
                MenuUpdate::Scroll
            } else {
                MenuUpdate::MoveCursorFrom(cursor)
            };
            match menu_state.page {
                MenuPage::Global {
                    ref mut cursor,
                    ref mut scroll,
                }
                | MenuPage::SubMenu {
                    ref mut cursor,
                    ref mut scroll,
                    ..
                } => {
                    *cursor = new_cursor;
                    *scroll = new_scroll;
                }
                _ => {}
            }
            (update, ConfigChange::None)
        }
    }
}

const NUM_LAST_GLOBAL: u8 = 2;

#[inline(never)]
fn edit_item(
    item: MenuItem,
    menu_state: &mut MenuState,
    config: &mut ClockConfig,
    channel_idx: u8,
    delta: i8,
    fast: bool,
) -> (MenuUpdate, ConfigChange) {
    let realign = ConfigChange::Realign(1 << channel_idx);
    let channel = &mut config.channels[channel_idx as usize];
    // undo acceleration for settings with few values
    let small_delta = if fast { delta / 4 } else { delta };
    let change = match item {
        MenuItem::Division => {
            let was_special = is_special_division(channel.division);
            channel.division = single_step_clock_division(channel.division, small_delta);
            if was_special != is_special_division(channel.division) {
                // the number of rows changed
                return (MenuUpdate::SwitchScreens, realign);
            }
            realign
        }
        MenuItem::Tuplet => {
            channel.tuplet = step_u8(channel.tuplet, small_delta, TUPLET_DOTTED);
            realign
        }
        MenuItem::PulseWidth => {
            channel.pulse_width = step_u8(channel.pulse_width, delta, 100);
            ConfigChange::Params
        }
        MenuItem::PhaseShift => {
            channel.phase_shift = channel.phase_shift.saturating_add(delta).clamp(-32, 32);
            realign
        }
        MenuItem::Swing => {
            channel.swing = step_u8(channel.swing, delta, 32);
            ConfigChange::Params
        }
        MenuItem::Probability => {
            channel.probability = step_u8(channel.probability, delta, 100);
            ConfigChange::Params
        }
        MenuItem::EuclidSteps => {
            let mut steps = step_u8(channel.euclid_steps, small_delta, MAX_EUCLID_STEPS);
            if steps == 1 {
                // 1 step is meaningless; jump between off and 2
                steps = if small_delta > 0 { 2 } else { 0 };
            }
            channel.euclid_steps = steps;
            channel.euclid_fill = channel.euclid_fill.min(steps);
            channel.euclid_rotate = channel.euclid_rotate.min(steps.saturating_sub(1));
            realign
        }
        MenuItem::EuclidFill => {
            channel.euclid_fill = step_u8(channel.euclid_fill, small_delta, channel.euclid_steps);
            realign
        }
        MenuItem::EuclidRotate => {
            channel.euclid_rotate = step_u8(
                channel.euclid_rotate,
                small_delta,
                channel.euclid_steps.saturating_sub(1),
            );
            realign
        }
        MenuItem::StopMode => {
            config.stop_mode = step_u8(config.stop_mode, small_delta, STOP_BAR);
            ConfigChange::Params
        }
        MenuItem::Load | MenuItem::Save => {
            menu_state.slot = step_u8(menu_state.slot, small_delta, NUM_PRESETS - 1);
            ConfigChange::None
        }
        MenuItem::Exit => return (MenuUpdate::NoUpdate, ConfigChange::None),
    };
    (MenuUpdate::UpdateValueAtCursor, change)
}
