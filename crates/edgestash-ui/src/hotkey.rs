use std::collections::BTreeMap;

use edgestash_ipc::config::{HotkeyConfig, HotkeySpec};

/// Identifies one of the 7 configurable global hotkey slots in [`HotkeyConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HotkeyField {
    StashLeft,
    StashRight,
    StashTop,
    StashBottom,
    ToggleActive,
    GroupActive,
    UnstashActive,
}

impl HotkeyField {
    pub const ALL: [Self; 7] = [
        Self::StashLeft,
        Self::StashRight,
        Self::StashTop,
        Self::StashBottom,
        Self::ToggleActive,
        Self::GroupActive,
        Self::UnstashActive,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::StashLeft => "Stash Left Edge",
            Self::StashRight => "Stash Right Edge",
            Self::StashTop => "Stash Top Edge",
            Self::StashBottom => "Stash Bottom Edge",
            Self::ToggleActive => "Toggle Active Drawer",
            Self::GroupActive => "Group Foreground Into Active Edge",
            Self::UnstashActive => "Unstash Active Edge Slot",
        }
    }

    pub fn get(self, cfg: &HotkeyConfig) -> &str {
        match self {
            Self::StashLeft => &cfg.stash_left,
            Self::StashRight => &cfg.stash_right,
            Self::StashTop => &cfg.stash_top,
            Self::StashBottom => &cfg.stash_bottom,
            Self::ToggleActive => &cfg.toggle_active,
            Self::GroupActive => &cfg.group_active,
            Self::UnstashActive => &cfg.unstash_active,
        }
    }

    pub fn set(self, cfg: &mut HotkeyConfig, value: String) {
        match self {
            Self::StashLeft => cfg.stash_left = value,
            Self::StashRight => cfg.stash_right = value,
            Self::StashTop => cfg.stash_top = value,
            Self::StashBottom => cfg.stash_bottom = value,
            Self::ToggleActive => cfg.toggle_active = value,
            Self::GroupActive => cfg.group_active = value,
            Self::UnstashActive => cfg.unstash_active = value,
        }
    }
}

/// Result of processing a keyboard event during interactive hotkey capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyCaptureOutcome {
    Pending,
    Cancelled,
    Captured(String),
}

/// Validates all 7 hotkey strings using [`HotkeySpec::parse`] and returns per-field errors.
pub fn validate_hotkey_config(cfg: &HotkeyConfig) -> BTreeMap<HotkeyField, String> {
    let mut errors = BTreeMap::new();
    for field in HotkeyField::ALL {
        let raw = field.get(cfg);
        if let Err(err) = HotkeySpec::parse(raw) {
            errors.insert(field, err);
        }
    }
    errors
}

/// Formats an `egui` modifier + key chord into EdgeStash canonical hotkey syntax.
pub fn format_captured_hotkey(modifiers: egui::Modifiers, key: egui::Key) -> Option<String> {
    let token = egui_key_to_token(key)?;
    let mut parts = Vec::with_capacity(4);
    if modifiers.ctrl || modifiers.command {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    parts.push(token);
    Some(parts.join("+"))
}

/// Processes a pressed key event while recording a hotkey (`Esc` cancels without modifiers).
pub fn process_captured_key(modifiers: egui::Modifiers, key: egui::Key) -> HotkeyCaptureOutcome {
    if key == egui::Key::Escape && !modifiers.ctrl && !modifiers.alt && !modifiers.shift {
        return HotkeyCaptureOutcome::Cancelled;
    }
    let has_mod = modifiers.ctrl || modifiers.command || modifiers.alt || modifiers.shift;
    if !has_mod && !is_function_key(key) {
        return HotkeyCaptureOutcome::Pending;
    }
    match format_captured_hotkey(modifiers, key) {
        Some(binding) if HotkeySpec::parse(&binding).is_ok() => {
            HotkeyCaptureOutcome::Captured(binding)
        }
        _ => HotkeyCaptureOutcome::Pending,
    }
}

fn is_function_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(
        key,
        F1 | F2
            | F3
            | F4
            | F5
            | F6
            | F7
            | F8
            | F9
            | F10
            | F11
            | F12
            | F13
            | F14
            | F15
            | F16
            | F17
            | F18
            | F19
            | F20
            | F21
            | F22
            | F23
            | F24
    )
}

fn egui_key_to_token(key: egui::Key) -> Option<&'static str> {
    use egui::Key::*;
    match key {
        ArrowLeft => Some("Left"),
        ArrowRight => Some("Right"),
        ArrowUp => Some("Up"),
        ArrowDown => Some("Down"),
        Backslash => Some("\\"),
        Space => Some("Space"),
        Tab => Some("Tab"),
        Escape => Some("Esc"),
        Enter => Some("Enter"),
        Num0 => Some("0"),
        Num1 => Some("1"),
        Num2 => Some("2"),
        Num3 => Some("3"),
        Num4 => Some("4"),
        Num5 => Some("5"),
        Num6 => Some("6"),
        Num7 => Some("7"),
        Num8 => Some("8"),
        Num9 => Some("9"),
        A => Some("A"),
        B => Some("B"),
        C => Some("C"),
        D => Some("D"),
        E => Some("E"),
        F => Some("F"),
        G => Some("G"),
        H => Some("H"),
        I => Some("I"),
        J => Some("J"),
        K => Some("K"),
        L => Some("L"),
        M => Some("M"),
        N => Some("N"),
        O => Some("O"),
        P => Some("P"),
        Q => Some("Q"),
        R => Some("R"),
        S => Some("S"),
        T => Some("T"),
        U => Some("U"),
        V => Some("V"),
        W => Some("W"),
        X => Some("X"),
        Y => Some("Y"),
        Z => Some("Z"),
        F1 => Some("F1"),
        F2 => Some("F2"),
        F3 => Some("F3"),
        F4 => Some("F4"),
        F5 => Some("F5"),
        F6 => Some("F6"),
        F7 => Some("F7"),
        F8 => Some("F8"),
        F9 => Some("F9"),
        F10 => Some("F10"),
        F11 => Some("F11"),
        F12 => Some("F12"),
        F13 => Some("F13"),
        F14 => Some("F14"),
        F15 => Some("F15"),
        F16 => Some("F16"),
        F17 => Some("F17"),
        F18 => Some("F18"),
        F19 => Some("F19"),
        F20 => Some("F20"),
        F21 => Some("F21"),
        F22 => Some("F22"),
        F23 => Some("F23"),
        F24 => Some("F24"),
        _ => None,
    }
}
