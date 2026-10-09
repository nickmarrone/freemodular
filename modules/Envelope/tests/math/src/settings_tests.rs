use crate::settings::*;

/// Raw ADC reading for a knob position in permille (the CV is inverted in hardware)
fn raw(permille: u32) -> u16 {
    MAX_ADC_VALUE - (permille * MAX_ADC_VALUE as u32 / 1000) as u16
}

#[test]
fn settings_round_trip() {
    let settings = Settings {
        mode: 6,
        long_range: true,
        attack_curve: 10,
        release_curve: 200,
        gate_behaviour: GateBehaviour::Cycle,
        aux_mode: Some(AuxMode::EndOfFallPulse),
    };
    assert_eq!(Settings::from_bytes(&settings.to_bytes(), 8), settings);
    assert_eq!(Settings::from_bytes(&Settings::DEFAULT.to_bytes(), 8), Settings::DEFAULT);
}

#[test]
fn legacy_saves_keep_mode_and_range_only() {
    // old firmware saved one byte; the rest is whatever follows it in EEPROM
    let old = [0x82, 0x03, 0x00, 0x01, 0x77, 0x02];
    let s = Settings::from_bytes(&old, 8);
    assert_eq!(s.mode, 2);
    assert!(s.long_range);
    assert_eq!(s.attack_curve, Settings::DEFAULT.attack_curve);
    assert_eq!(s.gate_behaviour, GateBehaviour::Continue);
    assert_eq!(s.aux_mode, None);
    // an erased EEPROM reads 0xFF
    assert_eq!(Settings::from_bytes(&[0xFF; SAVED_SIZE], 8).mode, 7);
    assert_eq!(Settings::from_bytes(&[0xFF; SAVED_SIZE], 4).mode, 0);
}

#[test]
fn invalid_values_fall_back_to_defaults() {
    let mut bytes = Settings::DEFAULT.to_bytes();
    bytes[4] = 9;
    bytes[5] = 6;
    let s = Settings::from_bytes(&bytes, 8);
    assert_eq!(s.gate_behaviour, GateBehaviour::Continue);
    assert_eq!(s.aux_mode, None);
}

#[test]
fn curve_is_linear_in_the_middle_and_full_at_the_ends() {
    assert_eq!(curve_amount(LINEAR_CURVE).0.to_bits(), 0);
    let mut s = Settings::DEFAULT;
    s.set_from_knob(0, &[raw(500), 0, 0, 0]);
    assert_eq!(curve_amount(s.attack_curve).0.to_bits(), 0);
    s.set_from_knob(0, &[raw(1000), 0, 0, 0]);
    let (c, negative) = curve_amount(s.attack_curve);
    assert!(!negative && c.to_bits() > 0xF000, "{c}");
    s.set_from_knob(0, &[raw(0), 0, 0, 0]);
    let (c, negative) = curve_amount(s.attack_curve);
    assert!(negative && c.to_bits() > 0xF000, "{c}");
    // monotonic across the knob
    let mut last = -1.0f64;
    for p in 0..=1000 {
        s.set_from_knob(1, &[0, raw(p), 0, 0]);
        let (c, negative) = curve_amount(s.release_curve);
        let v = if negative { -c.to_num::<f64>() } else { c.to_num::<f64>() };
        assert!(v >= last, "{p}");
        last = v;
    }
}

#[test]
fn discrete_settings_cover_the_knob() {
    let mut s = Settings::DEFAULT;
    let cv = |p| [0, 0, raw(p), raw(p)];
    s.set_from_knob(2, &cv(0));
    assert_eq!(s.gate_behaviour, GateBehaviour::Continue);
    s.set_from_knob(2, &cv(1000));
    assert_eq!(s.gate_behaviour, GateBehaviour::Cycle);
    s.set_from_knob(3, &cv(0));
    assert_eq!(s.aux_mode, Some(AuxMode::EndOfRise));
    s.set_from_knob(3, &cv(1000));
    assert_eq!(s.aux_mode, Some(AuxMode::EndOfFallPulse));
    assert_eq!(s.leds(3, AuxMode::EndOfRise), led(2) | led(3));
    s.set_from_knob(3, &cv(450));
    assert_eq!(s.aux_mode, Some(AuxMode::NonZero));
    s.set_from_knob(3, &cv(550));
    assert_eq!(s.aux_mode, Some(AuxMode::FollowGate));
}

#[test]
fn editor_ignores_quick_clicks_and_small_wobbles() {
    let mut e = Editor::new();
    let mut p = Pickup::new();
    e.press();
    // knob moves before the edit layer arms: not an edit
    assert_eq!(e.hold(100, &[100, 200, 300, 400]), None);
    assert_eq!(e.hold(100, &[500, 200, 300, 400]), None);
    assert!(!e.release(&[500, 200, 300, 400], &mut p));

    e.press();
    assert_eq!(e.hold(300, &[100, 200, 300, 400]), None);
    assert_eq!(e.hold(400, &[110, 190, 300, 420]), None);
    assert!(!e.release(&[110, 190, 300, 420], &mut p));
}

#[test]
fn editor_freezes_then_picks_up() {
    let mut e = Editor::new();
    let mut p = Pickup::new();
    e.press();
    e.hold(300, &[100, 200, 300, 400]);
    assert_eq!(e.hold(350, &[100, 200, 600, 400]), Some(2));
    assert_eq!(e.hold(360, &[100, 50, 600, 400]), Some(1));
    // still editing knob 1 even though knob 2 is further from where it started
    assert_eq!(e.hold(370, &[100, 40, 600, 400]), Some(1));
    let mut cv = [100, 40, 600, 400];
    e.freeze(&mut cv);
    assert_eq!(cv, [100, 200, 300, 400]);
    assert!(e.release(&[100, 40, 600, 400], &mut p));

    // after release, the edited knobs stay frozen until turned back through
    let mut cv = [100, 40, 600, 400];
    p.apply(&mut cv);
    assert_eq!(cv, [100, 200, 300, 400]);
    let mut cv = [100, 150, 400, 400];
    p.apply(&mut cv);
    assert_eq!(cv, [100, 200, 300, 400]);
    // knob 1 crosses its old value (jumping past it), knob 2 comes within tolerance
    let mut cv = [100, 260, 305, 400];
    p.apply(&mut cv);
    assert_eq!(cv, [100, 260, 305, 400]);
    // and both stay free afterwards
    let mut cv = [100, 10, 900, 400];
    p.apply(&mut cv);
    assert_eq!(cv, [100, 10, 900, 400]);
}

#[test]
fn knob_returned_to_its_start_is_not_frozen() {
    let mut e = Editor::new();
    let mut p = Pickup::new();
    e.press();
    e.hold(300, &[100, 200, 300, 400]);
    e.hold(350, &[500, 200, 300, 400]);
    assert!(e.release(&[102, 200, 300, 400], &mut p));
    let mut cv = [102, 200, 300, 400];
    p.apply(&mut cv);
    assert_eq!(cv, [102, 200, 300, 400]);
}
