//! Key bindings: parsing, platform defaults, chords and dispatch.
//!
//! Bindings are written as `"ctrl+shift+p"`, `"cmd+s"`, `"primary+k primary+b"`
//! (a two-step chord). `primary` means Ctrl on Linux/Windows and Cmd on macOS.
//! Key names follow egui's `Key::from_name` plus single characters.

use std::collections::HashMap;
use std::fmt;

use egui::{Key, Modifiers};
use serde::Deserialize;

use crate::command::Command;

/// Which UI element must own focus for a binding to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    /// Always applies.
    #[default]
    Global,
    /// An editor view has focus.
    Editor,
    /// A find/replace bar has focus.
    Find,
    /// Palette or other overlay has focus.
    Overlay,
    /// The file explorer sidebar has focus.
    Sidebar,
}

/// A single key press with modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

impl KeyPress {
    pub fn from_egui(key: Key, m: Modifiers) -> Self {
        Self { key, ctrl: m.ctrl, alt: m.alt, shift: m.shift, cmd: m.mac_cmd }
    }
}

impl fmt::Display for KeyPress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mac = cfg!(target_os = "macos");
        if self.ctrl {
            f.write_str(if mac { "⌃" } else { "Ctrl+" })?;
        }
        if self.alt {
            f.write_str(if mac { "⌥" } else { "Alt+" })?;
        }
        if self.shift {
            f.write_str(if mac { "⇧" } else { "Shift+" })?;
        }
        if self.cmd {
            f.write_str(if mac { "⌘" } else { "Super+" })?;
        }
        f.write_str(key_label(self.key))
    }
}

fn key_label(k: Key) -> &'static str {
    match k {
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::Enter => "Enter",
        Key::Space => "Space",
        Key::Backspace => "Backspace",
        Key::Delete => "Delete",
        Key::Escape => "Esc",
        Key::Tab => "Tab",
        _ => k.symbol_or_name(),
    }
}

/// One or more key presses in sequence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord(pub Vec<KeyPress>);

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, k) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{k}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyParseError {
    #[error("unknown key {0:?}")]
    UnknownKey(String),
    #[error("unknown modifier {0:?}")]
    UnknownModifier(String),
    #[error("empty key binding")]
    Empty,
}

/// Parse `"ctrl+shift+p"` or a chord like `"ctrl+k ctrl+b"`.
pub fn parse_chord(s: &str) -> Result<Chord, KeyParseError> {
    let mut presses = Vec::new();
    for part in s.split_whitespace() {
        presses.push(parse_press(part)?);
    }
    if presses.is_empty() {
        return Err(KeyParseError::Empty);
    }
    Ok(Chord(presses))
}

pub fn parse_press(s: &str) -> Result<KeyPress, KeyParseError> {
    let mut kp = KeyPress { key: Key::Space, ctrl: false, alt: false, shift: false, cmd: false };
    let s = s.trim();
    if s.is_empty() {
        return Err(KeyParseError::Empty);
    }
    // A trailing '+' is the key itself ("ctrl++").
    let (mods_str, key_name) = if let Some(stripped) = s.strip_suffix('+') {
        (stripped.trim_end_matches('+'), "+")
    } else {
        match s.rfind('+') {
            Some(i) => (&s[..i], &s[i + 1..]),
            None => ("", s),
        }
    };
    for m in mods_str.split('+').filter(|m| !m.is_empty()) {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => kp.ctrl = true,
            "alt" | "option" | "opt" => kp.alt = true,
            "shift" => kp.shift = true,
            "cmd" | "command" | "super" | "win" | "meta" => kp.cmd = true,
            "primary" => {
                if cfg!(target_os = "macos") {
                    kp.cmd = true
                } else {
                    kp.ctrl = true
                }
            }
            other => return Err(KeyParseError::UnknownModifier(other.into())),
        }
    }
    kp.key = parse_key(key_name).ok_or_else(|| KeyParseError::UnknownKey(key_name.into()))?;
    Ok(kp)
}

fn parse_key(s: &str) -> Option<Key> {
    let lower = s.to_ascii_lowercase();
    let named = match lower.as_str() {
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "esc" | "escape" => Some(Key::Escape),
        "enter" | "return" => Some(Key::Enter),
        "space" => Some(Key::Space),
        "backspace" => Some(Key::Backspace),
        "delete" | "del" => Some(Key::Delete),
        "insert" | "ins" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" | "pgup" => Some(Key::PageUp),
        "pagedown" | "pgdn" => Some(Key::PageDown),
        "tab" => Some(Key::Tab),
        "plus" | "+" | "=" | "equals" => Some(Key::Equals),
        "minus" | "-" => Some(Key::Minus),
        "comma" | "," => Some(Key::Comma),
        "period" | "." => Some(Key::Period),
        "slash" | "/" => Some(Key::Slash),
        "backslash" | "\\" => Some(Key::Backslash),
        "semicolon" | ";" => Some(Key::Semicolon),
        "quote" | "'" => Some(Key::Quote),
        "backtick" | "`" => Some(Key::Backtick),
        "[" | "openbracket" => Some(Key::OpenBracket),
        "]" | "closebracket" => Some(Key::CloseBracket),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if lower.len() == 1 {
        let c = lower.chars().next()?;
        if c.is_ascii_lowercase() {
            return Key::from_name(&c.to_ascii_uppercase().to_string());
        }
        if c.is_ascii_digit() {
            return Key::from_name(&c.to_string());
        }
    }
    Key::from_name(s).or_else(|| {
        // "f5" → "F5"
        Key::from_name(&(lower[..1].to_ascii_uppercase() + &lower[1..]))
    })
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub chord: Chord,
    pub command: Command,
    pub arg: Option<String>,
    pub context: Context,
}

/// The active key map plus pending chord state.
#[derive(Debug, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
    /// (first press, context) → true if some chord starts with this press.
    prefixes: HashMap<KeyPress, ()>,
    pending: Vec<KeyPress>,
}

/// Outcome of feeding a key press to the key map.
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch {
    /// No binding; let the key through to the widget.
    Unbound,
    /// The press began or continued a chord; wait for more.
    Pending,
    /// A binding matched.
    Command(Command, Option<String>),
}

#[derive(Deserialize)]
struct RawBinding {
    keys: String,
    command: String,
    #[serde(default)]
    arg: Option<String>,
    #[serde(default)]
    context: Context,
}

#[derive(Deserialize)]
struct RawKeymap {
    #[serde(default)]
    bindings: Vec<RawBinding>,
    /// Platform-specific extra sections; whichever matches is appended.
    #[serde(default)]
    linux: Vec<RawBinding>,
    #[serde(default)]
    windows: Vec<RawBinding>,
    #[serde(default)]
    macos: Vec<RawBinding>,
}

pub const DEFAULT_KEYMAP_TOML: &str = include_str!("../../../assets/keymap-default.toml");

impl Keymap {
    pub fn defaults() -> Keymap {
        let (km, errors) = Keymap::parse(DEFAULT_KEYMAP_TOML);
        for e in errors {
            tracing::error!("default keymap: {e}");
        }
        km
    }

    /// Parse a TOML key map, collecting (not failing on) bad entries so one
    /// typo cannot disable every shortcut.
    pub fn parse(src: &str) -> (Keymap, Vec<String>) {
        let mut errors = Vec::new();
        let raw: RawKeymap = match toml::from_str(src) {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("key map is not valid TOML: {e}"));
                return (Keymap::default(), errors);
            }
        };
        let mut km = Keymap::default();
        let platform = if cfg!(target_os = "macos") {
            raw.macos
        } else if cfg!(windows) {
            raw.windows
        } else {
            raw.linux
        };
        for rb in raw.bindings.into_iter().chain(platform) {
            let chord = match parse_chord(&rb.keys) {
                Ok(c) => c,
                Err(e) => {
                    errors.push(format!("binding {:?}: {e}", rb.keys));
                    continue;
                }
            };
            let Some(command) = Command::from_id(&rb.command) else {
                errors.push(format!("binding {:?}: unknown command {:?}", rb.keys, rb.command));
                continue;
            };
            km.add(Binding { chord, command, arg: rb.arg, context: rb.context });
        }
        (km, errors)
    }

    /// Layer `other` over `self`: later bindings win for the same chord/context.
    pub fn merge(&mut self, other: Keymap) {
        for b in other.bindings {
            self.add(b);
        }
    }

    pub fn add(&mut self, b: Binding) {
        // A later binding with the same chord and context replaces the earlier one.
        self.bindings.retain(|x| !(x.chord == b.chord && x.context == b.context));
        if b.chord.0.len() > 1 {
            self.prefixes.insert(b.chord.0[0], ());
        }
        self.bindings.push(b);
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Human-readable shortcut for a command, if bound (prefers editor/global).
    pub fn shortcut_for(&self, cmd: Command) -> Option<String> {
        self.bindings.iter().rev().find(|b| b.command == cmd && matches!(b.context, Context::Global | Context::Editor)).map(|b| b.chord.to_string())
    }

    /// Feed a key press. `active` lists the contexts currently in effect, most
    /// specific first (e.g. `[Editor, Global]`).
    pub fn dispatch(&mut self, press: KeyPress, active: &[Context]) -> Dispatch {
        self.pending.push(press);
        let seq = self.pending.clone();
        // Exact match in the most specific context first.
        for ctx in active {
            if let Some(b) = self.bindings.iter().rev().find(|b| b.context == *ctx && b.chord.0 == seq) {
                self.pending.clear();
                return Dispatch::Command(b.command, b.arg.clone());
            }
        }
        let is_prefix = self.bindings.iter().any(|b| active.contains(&b.context) && b.chord.0.len() > seq.len() && b.chord.0.starts_with(&seq));
        if is_prefix {
            return Dispatch::Pending;
        }
        self.pending.clear();
        Dispatch::Unbound
    }

    pub fn pending(&self) -> &[KeyPress] {
        &self.pending
    }

    pub fn cancel_pending(&mut self) {
        self.pending.clear();
    }

    /// Detect chords bound to more than one command in the same context.
    pub fn conflicts(&self) -> Vec<(Chord, Context, Vec<Command>)> {
        let mut map: HashMap<(Chord, Context), Vec<Command>> = HashMap::new();
        for b in &self.bindings {
            map.entry((b.chord.clone(), b.context)).or_default().push(b.command);
        }
        map.into_iter().filter(|(_, v)| v.len() > 1).map(|((c, ctx), v)| (c, ctx, v)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_presses_and_chords() {
        let p = parse_press("ctrl+shift+p").unwrap();
        assert!(p.ctrl && p.shift && !p.alt);
        assert_eq!(p.key, Key::P);
        let c = parse_chord("ctrl+k ctrl+b").unwrap();
        assert_eq!(c.0.len(), 2);
        assert_eq!(parse_press("ctrl++").unwrap().key, Key::Equals);
        assert_eq!(parse_press("f5").unwrap().key, Key::F5);
        assert_eq!(parse_press("ctrl+1").unwrap().key, Key::Num1);
        assert!(matches!(parse_press("hyper+x"), Err(KeyParseError::UnknownModifier(_))));
        assert!(matches!(parse_press("ctrl+nonsense"), Err(KeyParseError::UnknownKey(_))));
    }

    #[test]
    fn default_keymap_loads_without_errors() {
        let (km, errors) = Keymap::parse(DEFAULT_KEYMAP_TOML);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(km.bindings().len() > 50);
        assert!(km.conflicts().is_empty(), "{:?}", km.conflicts());
    }

    #[test]
    fn dispatch_handles_chords_and_contexts() {
        let (mut km, _) = Keymap::parse(
            r#"
            [[bindings]]
            keys = "ctrl+k ctrl+b"
            command = "view.toggle_sidebar"
            [[bindings]]
            keys = "ctrl+s"
            command = "file.save"
            context = "editor"
            [[bindings]]
            keys = "escape"
            command = "find.close"
            context = "find"
            "#,
        );
        let k = |s: &str| parse_press(s).unwrap();
        assert_eq!(km.dispatch(k("ctrl+k"), &[Context::Editor, Context::Global]), Dispatch::Pending);
        assert_eq!(km.dispatch(k("ctrl+b"), &[Context::Editor, Context::Global]), Dispatch::Command(Command::ToggleSidebar, None));
        assert_eq!(km.dispatch(k("ctrl+s"), &[Context::Editor, Context::Global]), Dispatch::Command(Command::Save, None));
        assert_eq!(km.dispatch(k("ctrl+s"), &[Context::Find, Context::Global]), Dispatch::Unbound);
        assert_eq!(km.dispatch(k("escape"), &[Context::Find, Context::Global]), Dispatch::Command(Command::CloseFind, None));
        assert_eq!(km.dispatch(k("ctrl+k"), &[Context::Global]), Dispatch::Pending);
        assert_eq!(km.dispatch(k("x"), &[Context::Global]), Dispatch::Unbound);
        assert!(km.pending().is_empty());
    }
}
