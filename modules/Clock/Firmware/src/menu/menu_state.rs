use crate::clock::{is_special_division, ClockConfig};
use crate::random::Rng;

pub enum MenuOrScreenSaverState {
    ScreenSaver(ScreenSaverState),
    Menu(MenuState),
}

#[repr(packed)]
pub struct ScreenSaverState {
    pub y_offsets: [u8; 16],
    pub color: bool,
    pub rng: Rng,
}

pub struct TapState {
    pub last_tap_ms: u32,
    pub intervals: [u16; 4],
    pub num_intervals: u8,
    pub next: u8,
}

pub struct MenuState {
    pub page: MenuPage,
    pub editing: EditingState,
    pub last_input_time_ms: u32,
    pub last_turn_time_ms: u32,
    /// preset slot selected on the global page
    pub slot: u8,
    pub tap: TapState,
}

impl MenuOrScreenSaverState {
    pub fn new(current_time_ms: u32) -> Self {
        MenuOrScreenSaverState::Menu(MenuState::new(current_time_ms))
    }
}

impl MenuState {
    pub fn new(time: u32) -> Self {
        MenuState {
            page: MenuPage::Bpm,
            editing: EditingState::Navigating,
            last_input_time_ms: time,
            last_turn_time_ms: time,
            slot: 0,
            tap: TapState {
                last_tap_ms: 0,
                intervals: [0; 4],
                num_intervals: 0,
                next: 0,
            },
        }
    }
}

impl ScreenSaverState {
    pub fn new(seed: u32) -> Self {
        Self {
            y_offsets: [0u8; 16],
            color: true,
            rng: Rng::new(seed),
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum EditingState {
    Navigating,
    Editing,
    /// BPM page only: editing tenths of a BPM
    EditingFine,
    /// BPM page only: encoder clicks are tempo taps
    Tap,
}

impl EditingState {
    pub fn toggle(&self) -> Self {
        match self {
            EditingState::Navigating => EditingState::Editing,
            _ => EditingState::Navigating,
        }
    }

    pub fn is_editing(&self) -> bool {
        *self != EditingState::Navigating
    }
}

pub enum MenuPage {
    /// Module-wide settings, above the BPM page
    Global { cursor: u8, scroll: u8 },
    Bpm,
    Main { cursor: u8 },
    SubMenu { cursor: u8, scroll: u8, channel: u8 },
    ConfirmReset,
}

#[derive(PartialEq, Eq)]
pub enum MenuUpdate {
    NoUpdate,
    UpdateValueAtCursor,
    ToggleEditingAtCursor,
    MoveCursorFrom(u8),
    Scroll,
    SwitchScreens,
    ScreenSaverStep(u8),
}

/// Rows of the scrolling list pages (channel detail page and global page)
#[derive(PartialEq, Eq, Clone, Copy)]
#[repr(u8)]
#[allow(dead_code)] // constructed by transmute
pub enum MenuItem {
    Division = 0,
    Tuplet,
    PulseWidth,
    PhaseShift,
    Swing,
    Probability,
    EuclidSteps,
    EuclidFill,
    EuclidRotate,
    Exit,
    StopMode,
    Load,
    Save,
}

const NUM_CHANNEL_ITEMS: u8 = MenuItem::Exit as u8 + 1;
const NUM_GLOBAL_ITEMS: u8 = 3;

impl MenuItem {
    fn from_u8(value: u8) -> Self {
        debug_assert!(value <= MenuItem::Save as u8);
        unsafe { core::mem::transmute(value) }
    }
}

impl MenuPage {
    /// Number of rows in a list page
    pub fn list_len(&self, config: &ClockConfig) -> u8 {
        match *self {
            MenuPage::Global { .. } => NUM_GLOBAL_ITEMS,
            MenuPage::SubMenu { channel, .. } => {
                if is_special_division(config.channels[channel as usize].division) {
                    2
                } else {
                    NUM_CHANNEL_ITEMS
                }
            }
            _ => 0,
        }
    }

    pub fn list_item(&self, index: u8, config: &ClockConfig) -> MenuItem {
        match *self {
            MenuPage::Global { .. } => MenuItem::from_u8(MenuItem::StopMode as u8 + index),
            _ => {
                if self.list_len(config) == 2 && index == 1 {
                    // special channels only have a tempo setting
                    MenuItem::Exit
                } else {
                    MenuItem::from_u8(index)
                }
            }
        }
    }
}
