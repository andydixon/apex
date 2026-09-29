#![no_main]
//! Settings, key map and theme parsing must never panic on any input and
//! must leave the editor with usable values.
use apex_ui::keymap::Keymap;
use apex_ui::settings::Settings;
use apex_ui::theme::Theme;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|src: &str| {
    let (s, _issues) = Settings::parse_layer(&Settings::default(), src, "fuzz");
    assert!(s.tab_width >= 1 && s.tab_width <= 16);
    assert!(s.font_size.is_finite());
    let _ = s.indent();
    let (_km, _errors) = Keymap::parse(src);
    let _ = Theme::parse(src);
});
