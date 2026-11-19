use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::RECT;
use windows::core::GUID;

/// Screen edge to which a stashed drawer or group is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl ScreenEdge {
    /// All four screen edges in deterministic clockwise order starting from Left.
    pub const ALL: [ScreenEdge; 4] = [
        ScreenEdge::Left,
        ScreenEdge::Top,
        ScreenEdge::Right,
        ScreenEdge::Bottom,
    ];

    /// Returns true if the edge runs vertically (Left or Right), where drawer size is width.
    pub fn is_vertical_edge(self) -> bool {
        matches!(self, ScreenEdge::Left | ScreenEdge::Right)
    }

    /// Returns true if the edge runs horizontally (Top or Bottom), where drawer size is height.
    pub fn is_horizontal_edge(self) -> bool {
        !self.is_vertical_edge()
    }
}

/// Visual transition animation style when opening or closing a drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationStyle {
    /// Smooth translation from screen bezel synced to `DwmFlush()`.
    Slide,
    /// Fades alpha `0 -> 255` while gliding `16px` into position.
    FloatAndFade,
    /// `0ms` instant appear/disappear.
    Instant,
}

/// Edge trigger mechanism configuration derived from user settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerGesture {
    /// Crisp bezel push on any stashed screen edge (dwell threshold).
    EdgeBump { dwell_ms: u64 },
    /// Windows 8 Charms style: hit corner `8x8px` box, then slide along edge.
    CornerSlide {
        corner_size_px: i32,
        slide_distance_px: i32,
        timeout_ms: u64,
    },
    /// Standard hover on `1px` edge strip with dwell time.
    EdgeHover { dwell_ms: u64 },
    /// Only open when clicking the edge indicator pill or pressing hotkey.
    ManualOrHotkeyOnly,
}

/// Presentation mode of an open drawer on the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockMode {
    /// Floats above windows (`HWND_TOPMOST`) and auto-hides on mouse-leave or focus-loss.
    FloatingOverlay,
    /// Permanent floating pin (`HWND_TOPMOST`, stays open until collapsed/unpinned, no backdrop dim).
    FloatingPinned,
    /// Reserves screen work area via `SHAppBarMessage` (pushes other windows aside permanently).
    NativeWorkAreaDock,
}

/// Global/default virtual desktop pinning behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VirtualDesktopMode {
    AllDesktops,
    CurrentDesktopOnly,
}

/// Per-slot virtual desktop pinning scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VirtualDesktopScope {
    /// Follows user across all Virtual Desktops.
    AllDesktops,
    /// Only triggers on the Virtual Desktop where it was originally stashed (raw 16-byte GUID).
    PinnedToDesktop([u8; 16]),
}

impl VirtualDesktopScope {
    /// Converts a Win32 `GUID` into a pinned scope.
    pub fn from_guid(guid: GUID) -> Self {
        Self::PinnedToDesktop(guid_to_bytes(guid))
    }

    /// Returns the pinned `GUID` if this scope is pinned to a specific desktop.
    pub fn as_guid(self) -> Option<GUID> {
        match self {
            Self::AllDesktops => None,
            Self::PinnedToDesktop(bytes) => Some(bytes_to_guid(bytes)),
        }
    }
}

/// Converts a `GUID` into deterministic little-endian byte storage.
pub fn guid_to_bytes(guid: GUID) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&guid.data1.to_le_bytes());
    out[4..6].copy_from_slice(&guid.data2.to_le_bytes());
    out[6..8].copy_from_slice(&guid.data3.to_le_bytes());
    out[8..16].copy_from_slice(&guid.data4);
    out
}

/// Reconstructs a Win32 `GUID` from serialized bytes.
pub fn bytes_to_guid(bytes: [u8; 16]) -> GUID {
    GUID {
        data1: u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes")),
        data2: u16::from_le_bytes(bytes[4..6].try_into().expect("2 bytes")),
        data3: u16::from_le_bytes(bytes[6..8].try_into().expect("2 bytes")),
        data4: bytes[8..16].try_into().expect("8 bytes"),
    }
}

/// Visual styling for the screen-edge indicator pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgePillStyle {
    MinimalLine,
    AppIconsStack,
    Hidden,
}

/// Directed corner-slide ("churn") zones around each monitor's four edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChurnZoneId {
    TopLeftDown,
    BottomLeftUp,
    TopRightDown,
    BottomRightUp,
    TopLeftRight,
    TopRightLeft,
    BottomLeftRight,
    BottomRightLeft,
}

impl ChurnZoneId {
    /// All 8 canonical directed corner-slide churn zones.
    pub const ALL: [Self; 8] = [
        Self::BottomRightUp,
        Self::TopRightDown,
        Self::BottomLeftUp,
        Self::TopLeftDown,
        Self::TopRightLeft,
        Self::TopLeftRight,
        Self::BottomRightLeft,
        Self::BottomLeftRight,
    ];

    /// Target edge associated with sliding along this corner corridor.
    pub const fn edge(self) -> ScreenEdge {
        match self {
            Self::TopLeftDown | Self::BottomLeftUp => ScreenEdge::Left,
            Self::TopRightDown | Self::BottomRightUp => ScreenEdge::Right,
            Self::TopLeftRight | Self::TopRightLeft => ScreenEdge::Top,
            Self::BottomLeftRight | Self::BottomRightLeft => ScreenEdge::Bottom,
        }
    }

    /// Whether entry starts in the far corner (`bottom` for Left/Right, `right` for Top/Bottom).
    pub const fn is_far_corner(self) -> bool {
        matches!(
            self,
            Self::BottomLeftUp | Self::BottomRightUp | Self::TopRightLeft | Self::BottomRightLeft
        )
    }

    /// Required bezel slide sign along the edge axis (`+1` = down/right, `-1` = up/left).
    pub const fn slide_sign(self) -> i32 {
        match self {
            Self::TopLeftDown | Self::TopRightDown | Self::TopLeftRight | Self::BottomLeftRight => {
                1
            }
            Self::BottomLeftUp
            | Self::BottomRightUp
            | Self::TopRightLeft
            | Self::BottomRightLeft => -1,
        }
    }

    /// Human-readable label shown in Settings UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::BottomRightUp => "Right Edge — Bottom -> Up (Upward Churn)",
            Self::TopRightDown => "Right Edge — Top -> Down (Downward Churn)",
            Self::BottomLeftUp => "Left Edge — Bottom -> Up (Upward Churn)",
            Self::TopLeftDown => "Left Edge — Top -> Down (Downward Churn)",
            Self::TopRightLeft => "Top Edge — Right -> Left (Leftward Churn)",
            Self::TopLeftRight => "Top Edge — Left -> Right (Rightward Churn)",
            Self::BottomRightLeft => "Bottom Edge — Right -> Left (Leftward Churn)",
            Self::BottomLeftRight => "Bottom Edge — Left -> Right (Rightward Churn)",
        }
    }
}

/// Serializable, platform-agnostic integer rectangle matching Win32 `RECT`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    /// Creates a new rectangle from edge coordinates.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Horizontal span (`right - left`), clamped to zero if inverted.
    pub fn width(self) -> i32 {
        (self.right - self.left).max(0)
    }

    /// Vertical span (`bottom - top`), clamped to zero if inverted.
    pub fn height(self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    /// Half-open Win32 containment (`left <= x < right && top <= y < bottom`).
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// Inclusive containment (`left <= x <= right && top <= y <= bottom`), matching `PtInRect`/margin hysteresis.
    pub fn contains_inclusive(self, x: i32, y: i32) -> bool {
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }

    /// Returns a new rectangle expanded outward on all four sides by `margin_px`.
    pub fn expanded(self, margin_px: i32) -> Self {
        Self {
            left: self.left - margin_px,
            top: self.top - margin_px,
            right: self.right + margin_px,
            bottom: self.bottom + margin_px,
        }
    }

    /// Returns the bounding rectangle containing both `self` and `other`.
    pub fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

impl From<Rect> for RECT {
    fn from(r: Rect) -> Self {
        RECT {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}
