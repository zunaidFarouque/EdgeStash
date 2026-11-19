use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::{
    AnimationStyle, ChurnZoneId, DockMode, EdgePillStyle, ScreenEdge, TriggerGesture,
    VirtualDesktopMode,
};

/// Top-level trigger mode stored in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GestureMode {
    #[default]
    EdgeBump,
    CornerSlide,
    EdgeHover,
    PillClick,
    HotkeyOnly,
}

/// Area-aware scroll mode determining when scrolling along the thin edge trigger strip cycles slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AreaAwareScrollMode {
    /// Works anywhere on the thin trigger line from initial contact/trigger until cursor leaves
    /// that line into the drawer window body; hovering the pill itself always cycles windows.
    InitialTriggerSession,
    /// Works any time the cursor rests on the thin trigger line (even after returning from the app window).
    AlwaysOnTriggerLine,
    /// Active strictly after the drawer was opened via edge trigger and before cursor leaves the thin line.
    #[default]
    OpenSessionStrict,
}

/// Assignment policy for an individual directed corner-slide churn zone.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChurnZoneAction {
    /// Open the default last-opened stashed drawer (or that edge's active stashed window).
    #[default]
    DefaultLastOpened,
    /// Open a specific stashed app matching `matcher` (exe name or window title substring) if stashed.
    SpecificApp { matcher: String },
    /// Ignore corner-slide gestures originating from this zone.
    Disabled,
}

/// Key modifier required when dragging a window to reveal stash drop zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DragDropModifierKey {
    #[default]
    Shift,
    Ctrl,
    Alt,
    Any,
}

/// Per-zone churn gesture bindings serialized cleanly under `[churn_zones]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChurnZonesConfig {
    pub top_left_down: ChurnZoneAction,
    pub bottom_left_up: ChurnZoneAction,
    pub top_right_down: ChurnZoneAction,
    pub bottom_right_up: ChurnZoneAction,
    pub top_left_right: ChurnZoneAction,
    pub top_right_left: ChurnZoneAction,
    pub bottom_left_right: ChurnZoneAction,
    pub bottom_right_left: ChurnZoneAction,
}

impl ChurnZonesConfig {
    /// Reads the action assigned to `zone`.
    pub fn get(&self, zone: ChurnZoneId) -> &ChurnZoneAction {
        match zone {
            ChurnZoneId::TopLeftDown => &self.top_left_down,
            ChurnZoneId::BottomLeftUp => &self.bottom_left_up,
            ChurnZoneId::TopRightDown => &self.top_right_down,
            ChurnZoneId::BottomRightUp => &self.bottom_right_up,
            ChurnZoneId::TopLeftRight => &self.top_left_right,
            ChurnZoneId::TopRightLeft => &self.top_right_left,
            ChurnZoneId::BottomLeftRight => &self.bottom_left_right,
            ChurnZoneId::BottomRightLeft => &self.bottom_right_left,
        }
    }

    /// Updates the action assigned to `zone`.
    pub fn set(&mut self, zone: ChurnZoneId, action: ChurnZoneAction) {
        let slot = match zone {
            ChurnZoneId::TopLeftDown => &mut self.top_left_down,
            ChurnZoneId::BottomLeftUp => &mut self.bottom_left_up,
            ChurnZoneId::TopRightDown => &mut self.top_right_down,
            ChurnZoneId::BottomRightUp => &mut self.bottom_right_up,
            ChurnZoneId::TopLeftRight => &mut self.top_left_right,
            ChurnZoneId::TopRightLeft => &mut self.top_right_left,
            ChurnZoneId::BottomLeftRight => &mut self.bottom_left_right,
            ChurnZoneId::BottomRightLeft => &mut self.bottom_right_left,
        };
        *slot = action;
    }
}

/// Configurable global hotkey strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub stash_left: String,
    pub stash_right: String,
    pub stash_top: String,
    pub stash_bottom: String,
    pub toggle_active: String,
    pub group_active: String,
    pub unstash_active: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            stash_left: "Ctrl+Alt+Shift+Left".to_string(),
            stash_right: "Ctrl+Alt+Shift+Right".to_string(),
            stash_top: "Ctrl+Alt+Shift+Up".to_string(),
            stash_bottom: "Ctrl+Alt+Shift+Down".to_string(),
            toggle_active: "Ctrl+Alt+\\".to_string(),
            group_active: "Ctrl+Alt+G".to_string(),
            unstash_active: "Ctrl+Alt+U".to_string(),
        }
    }
}

/// Complete configuration schema matching Section 5 of the architecture specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgeStashConfig {
    pub gesture_mode: GestureMode,
    pub edge_bump_dwell_ms: u64,
    pub corner_zone_px: i32,
    pub slide_distance_px: i32,
    pub corner_timeout_ms: u64,
    pub edge_dwell_ms: u64,
    pub disable_in_fullscreen: bool,

    pub transition_style: AnimationStyle,
    pub open_duration_ms: u32,
    pub close_duration_ms: u32,

    pub auto_focus_on_open: bool,
    pub hide_on_mouse_leave: bool,
    pub mouse_leave_delay_ms: u32,
    pub hide_on_focus_loss: bool,
    pub restore_prev_focus: bool,
    pub focus_restore_delay_ms: u32,

    pub dock_mode: DockMode,
    pub virtual_desktop_mode: VirtualDesktopMode,
    pub edge_pill_style: EdgePillStyle,
    pub auto_hide_pill: bool,
    pub show_attention_pill: bool,
    pub prioritize_attention_on_summon: bool,
    pub dim_background: u8,
    pub dim_fade_duration_ms: u32,
    pub lock_stashed_position: bool,
    pub area_aware_edge_scroll: bool,
    pub area_aware_edge_scroll_mode: AreaAwareScrollMode,
    pub scroll_trigger_line_only_when_open: bool,
    pub enable_drag_drop_zones: bool,
    pub drag_drop_zone_px: i32,
    pub drag_corner_zone_px: i32,
    pub drag_drop_dwell_ms: u32,
    pub drag_drop_left: bool,
    pub drag_drop_right: bool,
    pub drag_drop_top: bool,
    pub drag_drop_bottom: bool,
    pub drag_drop_require_modifier: bool,
    pub drag_drop_modifier_key: DragDropModifierKey,
    pub drag_drop_inset_px: i32,
    pub churn_diagonal_angle_deg: i32,
    pub churn_zones: ChurnZonesConfig,

    pub hotkeys: HotkeyConfig,
}

impl Default for EdgeStashConfig {
    fn default() -> Self {
        Self {
            gesture_mode: GestureMode::EdgeBump,
            edge_bump_dwell_ms: 80,
            corner_zone_px: 8,
            slide_distance_px: 40,
            corner_timeout_ms: 1000,
            edge_dwell_ms: 120,
            disable_in_fullscreen: true,

            transition_style: AnimationStyle::Slide,
            open_duration_ms: 160,
            close_duration_ms: 130,

            auto_focus_on_open: true,
            hide_on_mouse_leave: true,
            mouse_leave_delay_ms: 350,
            hide_on_focus_loss: true,
            restore_prev_focus: true,
            focus_restore_delay_ms: 50,

            dock_mode: DockMode::FloatingOverlay,
            virtual_desktop_mode: VirtualDesktopMode::AllDesktops,
            edge_pill_style: EdgePillStyle::AppIconsStack,
            auto_hide_pill: false,
            show_attention_pill: true,
            prioritize_attention_on_summon: true,
            dim_background: 0,
            dim_fade_duration_ms: 160,
            lock_stashed_position: true,
            area_aware_edge_scroll: true,
            area_aware_edge_scroll_mode: AreaAwareScrollMode::OpenSessionStrict,
            scroll_trigger_line_only_when_open: false,
            enable_drag_drop_zones: true,
            drag_drop_zone_px: 80,
            drag_corner_zone_px: 96,
            drag_drop_dwell_ms: 200,
            drag_drop_left: true,
            drag_drop_right: true,
            drag_drop_top: false,
            drag_drop_bottom: true,
            drag_drop_require_modifier: false,
            drag_drop_modifier_key: DragDropModifierKey::Shift,
            drag_drop_inset_px: 48,
            churn_diagonal_angle_deg: 45,
            churn_zones: ChurnZonesConfig::default(),

            hotkeys: HotkeyConfig::default(),
        }
    }
}

impl EdgeStashConfig {
    /// Returns whether drop zones are enabled for `edge`.
    pub fn is_drag_drop_edge_enabled(&self, edge: ScreenEdge) -> bool {
        match edge {
            ScreenEdge::Left => self.drag_drop_left,
            ScreenEdge::Right => self.drag_drop_right,
            ScreenEdge::Top => self.drag_drop_top,
            ScreenEdge::Bottom => self.drag_drop_bottom,
        }
    }

    /// Clamps numeric settings to the specification's safe bounds.
    pub fn validate_and_clamp(&mut self) {
        self.edge_bump_dwell_ms = self.edge_bump_dwell_ms.clamp(0, 1000);
        self.corner_zone_px = self.corner_zone_px.clamp(4, 32);
        self.slide_distance_px = self.slide_distance_px.clamp(16, 200);
        self.corner_timeout_ms = self.corner_timeout_ms.clamp(150, 3000);
        self.edge_dwell_ms = self.edge_dwell_ms.clamp(0, 1000);
        self.open_duration_ms = self.open_duration_ms.clamp(50, 500);
        self.close_duration_ms = self.close_duration_ms.clamp(50, 500);
        self.mouse_leave_delay_ms = self.mouse_leave_delay_ms.clamp(0, 5000);
        self.focus_restore_delay_ms = self.focus_restore_delay_ms.clamp(0, 2000);
        self.dim_background = self.dim_background.clamp(0, 60);
        self.dim_fade_duration_ms = self.dim_fade_duration_ms.clamp(0, 2000);
        self.churn_diagonal_angle_deg = self.churn_diagonal_angle_deg.clamp(15, 75);
        self.drag_drop_zone_px = self.drag_drop_zone_px.clamp(30, 240);
        self.drag_corner_zone_px = self.drag_corner_zone_px.clamp(40, 240);
        self.drag_drop_dwell_ms = self.drag_drop_dwell_ms.clamp(0, 1000);
        self.drag_drop_inset_px = self.drag_drop_inset_px.clamp(0, 200);
    }

    /// Parses a TOML configuration string, filling omitted keys with defaults and clamping ranges.
    pub fn from_toml_str(raw: &str) -> Result<Self, toml::de::Error> {
        let mut cfg: Self = toml::from_str(raw)?;
        cfg.validate_and_clamp();
        Ok(cfg)
    }

    /// Serializes the configuration into pretty TOML.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Converts the active gesture fields into a [`TriggerGesture`].
    pub fn trigger_gesture(&self) -> TriggerGesture {
        match self.gesture_mode {
            GestureMode::EdgeBump => TriggerGesture::EdgeBump {
                dwell_ms: self.edge_bump_dwell_ms,
            },
            GestureMode::CornerSlide => TriggerGesture::CornerSlide {
                corner_size_px: self.corner_zone_px,
                slide_distance_px: self.slide_distance_px,
                timeout_ms: self.corner_timeout_ms,
            },
            GestureMode::EdgeHover => TriggerGesture::EdgeHover {
                dwell_ms: self.edge_dwell_ms,
            },
            GestureMode::PillClick | GestureMode::HotkeyOnly => TriggerGesture::ManualOrHotkeyOnly,
        }
    }

    /// Default path `%APPDATA%\EdgeStash\config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|dir| dir.join("EdgeStash").join("config.toml"))
    }

    /// Default directory `%APPDATA%\EdgeStash\presets` for saved named configuration presets.
    pub fn default_presets_dir() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|dir| dir.join("EdgeStash").join("presets"))
    }

    /// Saves the current configuration into the presets directory with the given preset name.
    pub fn save_as_named_preset(&self, name: &str) -> io::Result<PathBuf> {
        let name = name.trim();
        if name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Preset name cannot be empty",
            ));
        }
        let invalid_chars = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
        if name.chars().any(|c| invalid_chars.contains(&c) || c.is_control()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Preset name contains invalid characters (\\ / : * ? \" < > |)",
            ));
        }

        let dir = Self::default_presets_dir().unwrap_or_else(|| PathBuf::from("presets"));
        fs::create_dir_all(&dir)?;
        let file_path = dir.join(format!("{}.toml", name));
        self.save_to_at(&file_path)?;
        Ok(file_path)
    }

    /// Lists all available `.toml` preset files found in the default presets directory.
    pub fn list_presets() -> Vec<(String, PathBuf)> {
        let mut presets = Vec::new();
        if let Some(dir) = Self::default_presets_dir()
            && let Ok(entries) = fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "toml")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    presets.push((stem.to_string(), path));
                }
            }
        }
        presets.sort_by_key(|a| a.0.to_lowercase());
        presets
    }

    /// Saves the configuration as pretty TOML to `path`, creating parent directories if needed.
    pub fn save_to_at(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = self.to_toml_string().map_err(io::Error::other)?;
        fs::write(path, content)
    }

    /// Loads config from `path`, creating a default config file if missing.
    pub fn load_or_create_at(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(raw) => Self::from_toml_str(&raw)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let cfg = Self::default();
                cfg.save_to_at(path)?;
                Ok(cfg)
            }
            Err(err) => Err(err),
        }
    }
}

/// Supported primary virtual keys for global hotkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualKey {
    Left,
    Right,
    Up,
    Down,
    Backslash,
    Space,
    Tab,
    Escape,
    Enter,
    Char(u8),
    Function(u8),
}

/// Parsed hotkey specification ready for Win32 `RegisterHotKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: VirtualKey,
}

impl HotkeySpec {
    /// Parses a binding string like `"Ctrl+Alt+Shift+Left"` or `"Ctrl+Alt+\\"`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("empty hotkey binding".to_string());
        }

        let mut spec = Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: VirtualKey::Space,
        };
        let mut parsed_key: Option<VirtualKey> = None;

        for token in trimmed.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let lower = token.to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => spec.ctrl = true,
                "alt" | "menu" => spec.alt = true,
                "shift" => spec.shift = true,
                "win" | "super" | "meta" => spec.win = true,
                _ => {
                    if parsed_key.is_some() {
                        return Err(format!("multiple primary keys in hotkey '{raw}'"));
                    }
                    parsed_key = Some(parse_primary_key(token)?);
                }
            }
        }

        match parsed_key {
            Some(key) => {
                spec.key = key;
                Ok(spec)
            }
            None => Err(format!("missing primary key in hotkey '{raw}'")),
        }
    }

    /// Returns the Win32 Virtual-Key (`VK_*`) numeric code.
    pub fn vk_code(self) -> u32 {
        match self.key {
            VirtualKey::Left => 0x25,
            VirtualKey::Up => 0x26,
            VirtualKey::Right => 0x27,
            VirtualKey::Down => 0x28,
            VirtualKey::Backslash => 0xDC, // VK_OEM_5
            VirtualKey::Space => 0x20,
            VirtualKey::Tab => 0x09,
            VirtualKey::Escape => 0x1B,
            VirtualKey::Enter => 0x0D,
            VirtualKey::Char(b) => b as u32,
            VirtualKey::Function(n) => 0x70 + u32::from(n - 1),
        }
    }

    /// Returns the Win32 `MOD_*` modifier bitmask (`MOD_ALT=1`, `MOD_CONTROL=2`, `MOD_SHIFT=4`, `MOD_WIN=8`, `MOD_NOREPEAT=0x4000`).
    pub fn win32_modifiers(self) -> u32 {
        let mut mods = 0x4000u32; // MOD_NOREPEAT
        if self.alt {
            mods |= 0x0001;
        }
        if self.ctrl {
            mods |= 0x0002;
        }
        if self.shift {
            mods |= 0x0004;
        }
        if self.win {
            mods |= 0x0008;
        }
        mods
    }
}

fn parse_primary_key(token: &str) -> Result<VirtualKey, String> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "left" => Ok(VirtualKey::Left),
        "right" => Ok(VirtualKey::Right),
        "up" => Ok(VirtualKey::Up),
        "down" => Ok(VirtualKey::Down),
        "\\" | "backslash" | "oem_5" => Ok(VirtualKey::Backslash),
        "space" => Ok(VirtualKey::Space),
        "tab" => Ok(VirtualKey::Tab),
        "esc" | "escape" => Ok(VirtualKey::Escape),
        "enter" | "return" => Ok(VirtualKey::Enter),
        _ => {
            if lower.len() == 1 {
                let b = lower.as_bytes()[0].to_ascii_uppercase();
                if b.is_ascii_alphanumeric() {
                    return Ok(VirtualKey::Char(b));
                }
            }
            if let Some(rest) = lower.strip_prefix('f')
                && let Ok(n) = rest.parse::<u8>()
                && (1..=24).contains(&n)
            {
                return Ok(VirtualKey::Function(n));
            }
            Err(format!("unsupported key '{token}'"))
        }
    }
}
