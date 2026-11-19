# EdgeStash: Universal Windows Edge-Drawer & Stash Engine
**Target OS:** Windows 10 (1809+) & Windows 11 (21H2–25H2+)  
**Language & Toolchain:** Rust (2024 Edition, MSVC ABI) + `windows` crate (`v0.58+`)  
**Performance Budget:** `< 3 MB` Private Working Set (Idle Daemon), `0.00%` Idle CPU, Frame-locked `DwmFlush()` animations (`60Hz–360Hz`).

---

## 1. Product Vision & Workflow Lifecycle Analysis

Unlike tiling window managers (GlazeWM, Komorebi) that force keyboard-centric paradigms, or Slidepad which only hosts web views, **EdgeStash** treats *any* arbitrary native Win32/UWP/PWA/Electron window (`HWND`) as a summonable edge drawer designed equally for **Mouse-First** and **Keyboard-First** workflows.

### 1.1 The Four Lifecycle Phases of a Stashed Window

#### Phase A: Capture & Registration (Mouse + Keyboard Parity)
Users can stash any window into an edge slot (`Left`, `Right`, `Top`, `Bottom`) via three intuitive methods:
1. **Mouse Drag-to-Edge Gesture:** Hold a modifier (e.g., `Right-Click` while dragging a title bar, or drag to a `4px` edge zone and hold for `350ms`). A subtle translucent Direct2D preview pill appears indicating the window will be stashed rather than Aero-snapped.
2. **Titlebar / System Tray Context Action:** Middle-clicking a window's minimize button or selecting from the EdgeStash floating pill menu.
3. **Global Hotkey:** Pressing `Win + Shift + [Arrow Key]` immediately captures the foreground window onto that screen edge.

#### Phase B: Dormant (Stashed) State
* The target `HWND` is **not minimized** (`SW_MINIMIZE` causes Chromium/Electron and WinUI apps to suspend their GPU swapchains, resulting in a 100–250ms white/black flash when restored).
* Instead, the window is parked off-screen and **DWM-Cloaked** (`DWMWA_CLOAK`), removing it from the compositor render pass and multi-monitor spillover while keeping its surface warm in VRAM.
* Its taskbar button and `Alt+Tab` entry are hidden (`ITaskbarList::DeleteTab` + `WS_EX_TOOLWINDOW` management) so it acts as a true background appliance.

#### Phase C: Summoning & Presentation Modes
Users can configure per-window or global summoning behavior:
1. **Trigger Mechanisms:**
   * **Charms Gesture (Zero False-Positives):** Move cursor into a `8x8px` corner zone (e.g., top-right) and slide downward along the bezel for `> 40px` within `600ms`. Prevents accidental triggers when clicking a maximized window's `X` close button or scrollbar.
   * **Edge Hover with Dwell Delay:** Rest cursor on the `1px` edge trigger strip for a configurable dwell time (e.g., `120ms`).
   * **Visual Pill Tab Click/Scroll:** A tiny, customizable translucent pill indicator on the screen edge. Hovering or clicking opens the active drawer; scrolling the mouse wheel over the pill cycles through stacked windows on that edge.
   * **Instant Hotkey Toggle:** e.g., `Win + \` or `Alt + Space` summons the drawer and immediately focuses it for typing.
2. **Animation Styles (Configurable):**
   * **Slide Mode:** Window translates smoothly from the bezel along the X/Y axis using a cubic-bezier easing curve synced to `DwmFlush()`.
   * **Float + Fade Mode:** Window spawns `16px` away from its final resting spot with `0%` opacity (`WS_EX_LAYERED`) and simultaneously fades to `100%` opacity while gliding the short `16px` distance into place over `140ms`.
   * **Instant Snap Mode:** `0ms` latency transition for ultra-fast keyboard workflows.
3. **Display Modes (Overlay vs. Native Docked):**
   * **Floating Overlay Mode (Default):** Floats (`HWND_TOPMOST`) over existing windows without resizing the desktop workspace.
   * **Native Docked Mode ("Everything Moves Left/Right"):** Registers an OS-level Application Desktop Toolbar (`SHAppBarMessage`) for that edge. Windows automatically shrinks the monitor's usable work area (`SPI_GETWORKAREA`), pushing all maximized and snapped windows aside cleanly—just like a native docked sidebar.

#### Phase D: Dismissal & Focus Restoration Lifecycle
When a drawer is open in Floating Overlay Mode, how and when it closes—and where keyboard focus returns—must feel telepathic:
1. **Prior Foreground Tracking:** Before summoning or focusing a stashed window, the engine records `previous_foreground_hwnd = GetForegroundWindow()`.
2. **Mouse-Leave Dismissal (with Hysteresis & Popup Guard):**
   * Once the cursor enters the stashed window (`has_entered_window = true`), a lightweight `30Hz` check monitors cursor bounds.
   * If the cursor leaves the window bounds + a `24px` safety margin, a configurable countdown starts (`auto_hide_delay_ms`, e.g., `350ms`). If the cursor re-enters before expiration, the timer cancels.
   * **Popup/Context Menu Guard:** If the user right-clicks inside the stashed window or opens a dropdown/file-picker (`GetLastActivePopup(stashed_hwnd) != stashed_hwnd` or an active menu class `#32768` owned by the target process exists), auto-hide is paused until the popup closes.
3. **Focus-Loss Dismissal:**
   * If the user clicks outside the stashed window into their IDE or browser (`EVENT_SYSTEM_FOREGROUND` hook), the drawer immediately slides away without waiting for the mouse-leave delay.
4. **Deterministic Focus Restoration:**
   * When a drawer auto-hides after the mouse leaves, configurable behavior (`restore_focus_on_mouse_leave: bool`, `focus_restore_delay_ms: u32`) restores keyboard focus directly back to `previous_foreground_hwnd` so the user can immediately resume typing in their code editor without clicking.

---

## 2. Virtual Desktop Lifecycle Architecture

Windows 10/11 Virtual Desktops notoriously break naive window managers because undocumented internal COM GUIDs (`IVirtualDesktopManagerInternal`) change across almost every Windows Insider build.

**The Bulletproof Solution (100% Documented COM API):**
We use the official, stable `IVirtualDesktopManager` (`CLSID_VirtualDesktopManager: {AA509086-5CA9-4C25-8F95-589D3C07B48A}`) combined with state tracking:
* **Mode 1: "Follow All Virtual Desktops" (Global Pin):**
  When the user switches Virtual Desktops and triggers the edge drawer, EdgeStash queries the current active desktop GUID (via a hidden helper window owned by our process on the current desktop using `GetWindowDesktopId(helper_hwnd)`) and calls `IVirtualDesktopManager::MoveWindowToDesktop(stashed_hwnd, &current_desktop_guid)` right before uncloaking and animating the window in.
* **Mode 2: "Pin to Specific Virtual Desktop":**
  When a window is stashed, EdgeStash calls `GetWindowDesktopId(stashed_hwnd)` and stores that `GUID` in the stash slot's metadata. When the user switches to a different Virtual Desktop, `IsWindowOnCurrentVirtualDesktop` returns `false`, and that slot's edge trigger is automatically muted until they return to that desktop.

---

## 3. System Architecture & Crate Layout

To guarantee `< 3 MB` idle RAM, the project is structured as a Cargo Workspace with strict separation between the resident Win32 daemon and the on-demand GUI configurator:

```text
edgestash/
├── Cargo.toml                  # Workspace root
├── crates/
│   ├── edgestash-daemon/       # Pure Win32/DWM/COM daemon (~2 MB RAM, 0% idle CPU)
│   │   ├── src/
│   │   │   ├── main.rs         # Win32 message pump & single-instance mutex
│   │   │   ├── state.rs        # Slot manager, HWND registry, crash-recovery manifest
│   │   │   ├── trigger.rs      # 1px edge trigger windows & Win8 Charms state machine
│   │   │   ├── animator.rs     # DwmFlush() VSync-locked Slide & Float+Fade engine
│   │   │   ├── dwm.rs          # Extended frame bounds, DWMWA_CLOAK, shadow math
│   │   │   ├── focus.rs        # WinEventHook foreground tracker & focus restorer
│   │   │   ├── appbar.rs       # SHAppBarMessage native workspace docking
│   │   │   └── vdesktop.rs     # IVirtualDesktopManager COM wrapper
│   ├── edgestash-ipc/          # Named-pipe / shared-memory IPC & TOML config schema
│   └── edgestash-ui/           # Separate binary launched ONLY when "Settings" is opened
```

---

## 4. Core Engine Implementation Specification (Rust + `windows-rs`)

* Coding AI Instructions: Do **not** alter the Win32 flag combinations, DWM attribute constants, or focus-restoration thread attachment logic below. These patterns are specifically engineered to prevent Win32 deadlocks, multi-monitor spillover, and Windows 11 border misalignment.*

### 4.1 `Cargo.toml` Dependencies for `edgestash-daemon`

```toml
[package]
name = "edgestash-daemon"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
toml = "0.8"

[dependencies.windows]
version = "0.58"
features = [
    "Win32_Foundation",
    "Win32_Graphics_Dwm",
    "Win32_Graphics_Gdi",
    "Win32_System_Com",
    "Win32_System_LibraryLoader",
    "Win32_System_Threading",
    "Win32_UI_Accessibility",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_Shell",
    "Win32_UI_WindowsAndMessaging",
]
```

### 4.2 Core Data Structures & Configuration Schema (`src/state.rs`)

```rust
use windows::Win32::Foundation::{HWND, RECT};
use windows::core::GUID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationStyle {
    /// Smooth translation from screen bezel synced to DwmFlush()
    Slide,
    /// Fades alpha 0 -> 255 while gliding 16px into position
    FloatAndFade,
    /// 0ms instant appear/disappear
    Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerGesture {
    /// Windows 8 Charms style: Hit corner 8x8px box, then slide along edge
    CornerSlide { corner_size_px: i32, slide_distance_px: i32, timeout_ms: u64 },
    /// Standard hover on 1px edge strip with dwell time
    EdgeHover { dwell_ms: u64 },
    /// Only open when clicking the edge indicator pill or pressing hotkey
    ManualOrHotkeyOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enumDockMode {
    /// Floats above windows (HWND_TOPMOST)
    FloatingOverlay,
    /// Reserves screen work area via SHAppBarMessage (pushes other windows aside)
    NativeWorkAreaDock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VirtualDesktopScope {
    /// Follows user across all Virtual Desktops
    AllDesktops,
    /// Only triggers on the Virtual Desktop where it was originally stashed
    PinnedToDesktop(GUID),
}

#[derive(Debug, Clone)]
pub struct StashedWindow {
    pub hwnd: HWND,
    pub original_ex_style: isize,
    pub original_rect: RECT,
    pub edge: ScreenEdge,
    pub monitor_rect: RECT,
    pub width_or_height_px: i32,
    pub offset_along_edge_px: i32,
    pub animation_style: AnimationStyle,
    pub animation_duration_ms: u32,
    pub trigger_gesture: TriggerGesture,
    pub dock_mode: enumDockMode,
    pub vd_scope: VirtualDesktopScope,
    pub auto_hide_on_mouse_leave: bool,
    pub mouse_leave_delay_ms: u32,
    pub restore_previous_focus: bool,
    pub focus_restore_delay_ms: u32,
    pub is_currently_open: bool,
    pub has_cursor_entered_since_open: bool,
}
```

### 4.3 Windows 11 Invisible Shadow Compensation & DWM Cloaking (`src/dwm.rs`)

Windows 10/11 windows include an invisible ~7px drop-shadow border around the left, right, and bottom edges (`GetWindowRect` includes this shadow; `DWMWA_EXTENDED_FRAME_BOUNDS` returns the visible glass edge). Without this math, stashed windows have an ugly 7px gap from the monitor bezel.

```rust
use std::mem::size_of;
use windows::Win32::Foundation::{BOOL, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::core::Result;

#[derive(Debug, Clone, Copy, Default)]
pub struct BorderInsets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Calculates the exact invisible DWM shadow border thickness in physical pixels.
pub fn get_invisible_border_insets(hwnd: HWND) -> Result<BorderInsets> {
    unsafe {
        let mut window_rect = RECT::default();
        GetWindowRect(hwnd, &mut window_rect)?;

        let mut frame_rect = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame_rect as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )?;

        Ok(BorderInsets {
            left: frame_rect.left - window_rect.left,
            top: frame_rect.top - window_rect.top,
            right: window_rect.right - frame_rect.right,
            bottom: window_rect.bottom - frame_rect.bottom,
        })
    }
}

/// Cloaks or uncloaks a window via DWM.
/// A cloaked window remains warm in GPU memory (no redraw flash on uncloak),
/// consumes 0 compositor bandwidth, and NEVER bleeds onto adjacent monitors!
pub fn set_window_cloaked(hwnd: HWND, cloaked: bool) -> Result<()> {
    unsafe {
        let value: BOOL = BOOL::from(cloaked);
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            &value as *const _ as *const _,
            size_of::<BOOL>() as u32,
        )
    }
}
```

### 4.4 VSync-Locked 120Hz/240Hz Animator: Slide & Float+Fade (`src/animator.rs`)

Never use `std::thread::sleep(16)` for window animations—timer drift causes micro-stuttering on high-refresh-rate monitors. Calling `DwmFlush()` blocks the animation fiber until the exact microsecond the Desktop Window Manager presents the next VSync frame, automatically adapting to 60Hz, 120Hz, 144Hz, or 360Hz displays.

```rust
use std::time::Instant;
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    WS_EX_LAYERED,
};
use crate::dwm::{get_invisible_border_insets, set_window_cloaked};
use crate::state::AnimationStyle;

#[inline]
fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Animates a stashed window into or out of view, compensating for Win11 invisible borders.
pub fn animate_window_transition(
    hwnd: HWND,
    start_visible_x: i32,
    start_visible_y: i32,
    end_visible_x: i32,
    end_visible_y: i32,
    duration_ms: u32,
    style: AnimationStyle,
    is_opening: bool,
) {
    let insets = get_invisible_border_insets(hwnd).unwrap_or_default();
    let start_x = start_visible_x - insets.left;
    let start_y = start_visible_y - insets.top;
    let end_x = end_visible_x - insets.left;
    let end_y = end_visible_y - insets.top;

    unsafe {
        if is_opening {
            // Uncloak before first frame
            let _ = set_window_cloaked(hwnd, false);
        }

        if style == AnimationStyle::Instant || duration_ms == 0 {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                end_x,
                end_y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
            if !is_opening {
                let _ = set_window_cloaked(hwnd, true);
            }
            return;
        }

        let orig_ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let had_layered = (orig_ex_style & (WS_EX_LAYERED.0 as isize)) != 0;

        if style == AnimationStyle::FloatAndFade && !had_layered {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, orig_ex_style | (WS_EX_LAYERED.0 as isize));
        }

        let start_time = Instant::now();
        loop {
            let elapsed = start_time.elapsed().as_secs_f32() * 1000.0;
            let raw_t = (elapsed / duration_ms as f32).clamp(0.0, 1.0);
            let eased_t = ease_out_cubic(raw_t);

            match style {
                AnimationStyle::Slide => {
                    let cur_x = start_x + ((end_x - start_x) as f32 * eased_t).round() as i32;
                    let cur_y = start_y + ((end_y - start_y) as f32 * eased_t).round() as i32;
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        cur_x,
                        cur_y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                AnimationStyle::FloatAndFade => {
                    // Float only 16px along the movement vector while fading 0..255
                    let dx = (end_x - start_x).signum() * 16;
                    let dy = (end_y - start_y).signum() * 16;
                    let (float_start_x, float_start_y, float_end_x, float_end_y) = if is_opening {
                        (end_x - dx, end_y - dy, end_x, end_y)
                    } else {
                        (start_x, start_y, start_x - dx, start_y - dy)
                    };

                    let cur_x = float_start_x + ((float_end_x - float_start_x) as f32 * eased_t).round() as i32;
                    let cur_y = float_start_y + ((float_end_y - float_start_y) as f32 * eased_t).round() as i32;
                    let alpha = if is_opening {
                        (255.0 * eased_t) as u8
                    } else {
                        (255.0 * (1.0 - eased_t)) as u8
                    };

                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        cur_x,
                        cur_y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                AnimationStyle::Instant => unreachable!(),
            }

            if raw_t >= 1.0 {
                break;
            }

            // Lock loop iteration to exact DWM VSync presentation
            let _ = DwmFlush();
        }

        // Clean up WS_EX_LAYERED after fade-in so hardware-accelerated apps run uninhibited
        if style == AnimationStyle::FloatAndFade && !had_layered {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, orig_ex_style);
        }

        if !is_opening {
            // Park off-screen and cloak so multi-monitor setups never see it
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                end_x,
                end_y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = set_window_cloaked(hwnd, true);
        }
    }
}
```

### 4.5 Deterministic Focus Restoration & Popup Guard (`src/focus.rs`)

Windows actively prevents background processes from stealing focus via `SetForegroundWindow` (the infamous flashing taskbar icon bug). To deterministically restore focus to the user's previous window (`previous_foreground_hwnd`) when their mouse leaves the stashed drawer, we use the `AttachThreadInput` bridge + Popup Guard.

```rust
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetLastActivePopup, GetWindowRect,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetForegroundWindow,
};

/// Checks if the stashed window currently has an open context menu, dropdown, or modal dialog.
/// If true, EdgeStash MUST NOT auto-hide the drawer even if the cursor leaves the main rect.
pub fn has_active_child_popup(stashed_hwnd: HWND) -> bool {
    unsafe {
        let popup = GetLastActivePopup(stashed_hwnd);
        if popup != stashed_hwnd && IsWindowVisible(popup).as_bool() {
            return true;
        }
        false
    }
}

/// Checks if the cursor is inside the stashed window + a hysteresis margin (e.g. 24px).
pub fn is_cursor_inside_with_margin(hwnd: HWND, margin_px: i32) -> bool {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        pt.x >= (rect.left - margin_px)
            && pt.x <= (rect.right + margin_px)
            && pt.y >= (rect.top - margin_px)
            && pt.y <= (rect.bottom + margin_px)
    }
}

/// Reliably restores keyboard focus to `target_hwnd` without triggering Windows Foreground Lock.
pub fn force_restore_foreground_window(target_hwnd: HWND) {
    unsafe {
        if !IsWindow(target_hwnd).as_bool() || !IsWindowVisible(target_hwnd).as_bool() {
            return;
        }

        let current_fg = GetForegroundWindow();
        if current_fg == target_hwnd {
            return;
        }

        let fg_thread = GetWindowThreadProcessId(current_fg, None);
        let current_thread = GetCurrentThreadId();

        if fg_thread != 0 && fg_thread != current_thread {
            let _ = AttachThreadInput(current_thread, fg_thread, true);
            let _ = SetForegroundWindow(target_hwnd);
            let _ = SetFocus(target_hwnd);
            let _ = AttachThreadInput(current_thread, fg_thread, false);
        } else {
            let _ = SetForegroundWindow(target_hwnd);
        }
    }
}
```

### 4.6 Native Workspace Docking ("Everything Moves Left/Right") (`src/appbar.rs`)

When the user pins a stashed window into **Native Docked Mode**, EdgeStash registers an OS AppBar on that edge so Windows shrinks the desktop work area and automatically pushes all maximized/snapped windows out of the way.

```rust
use std::mem::size_of;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_NEW, ABM_QUERYPOS,
    ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use crate::state::ScreenEdge;

pub fn register_or_update_native_dock(
    helper_hwnd: HWND,
    edge: ScreenEdge,
    monitor_rect: RECT,
    dock_thickness_px: i32,
    enable: bool,
) {
    unsafe {
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: helper_hwnd,
            ..Default::default()
        };

        if !enable {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            return;
        }

        SHAppBarMessage(ABM_NEW, &mut abd);

        abd.uEdge = match edge {
            ScreenEdge::Left => ABE_LEFT,
            ScreenEdge::Right => ABE_RIGHT,
            ScreenEdge::Top => ABE_TOP,
            ScreenEdge::Bottom => ABE_BOTTOM,
        };

        abd.rc = monitor_rect;
        match edge {
            ScreenEdge::Left => abd.rc.right = abd.rc.left + dock_thickness_px,
            ScreenEdge::Right => abd.rc.left = abd.rc.right - dock_thickness_px,
            ScreenEdge::Top => abd.rc.bottom = abd.rc.top + dock_thickness_px,
            ScreenEdge::Bottom => abd.rc.top = abd.rc.bottom - dock_thickness_px,
        }

        // Ask Explorer to validate and reserve the workspace rectangle
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        SHAppBarMessage(ABM_SETPOS, &mut abd);
    }
}
```

### 4.7 Virtual Desktop Pinning Engine (`src/vdesktop.rs`)

```rust
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::core::{Result, GUID};

pub struct VirtualDesktopController {
    vdm: IVirtualDesktopManager,
}

impl VirtualDesktopController {
    pub fn new() -> Result<Self> {
        unsafe {
            let vdm: IVirtualDesktopManager =
                CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)?;
            Ok(Self { vdm })
        }
    }

    pub fn is_on_current_desktop(&self, hwnd: HWND) -> bool {
        unsafe {
            self.vdm
                .IsWindowOnCurrentVirtualDesktop(hwnd)
                .map(|b| b.as_bool())
                .unwrap_or(true)
        }
    }

    pub fn get_window_desktop_id(&self, hwnd: HWND) -> Option<GUID> {
        unsafe { self.vdm.GetWindowDesktopId(hwnd).ok() }
    }

    /// Pulls a globally-pinned stashed window onto the user's current active Virtual Desktop
    /// right before uncloaking and animating it.
    pub fn move_window_to_desktop(&self, hwnd: HWND, desktop_id: &GUID) -> Result<()> {
        unsafe { self.vdm.MoveWindowToDesktop(hwnd, desktop_id) }
    }
}
```

---

## 5. Extended Customizability Matrix

All settings are serialized in `%APPDATA%\EdgeStash\config.toml` with live file-watcher reload (`ReadDirectoryChangesW`):

| Category | Setting Key | Options / Range | Default |
| :--- | :--- | :--- | :--- |
| **Triggering** | `gesture_mode` | `CornerSlide`, `EdgeHover`, `PillClick`, `HotkeyOnly` | `CornerSlide` |
| **Triggering** | `corner_zone_px` | `4..32` px | `8` px |
| **Triggering** | `edge_dwell_ms` | `0..1000` ms | `120` ms |
| **Triggering** | `disable_in_fullscreen` | `true` / `false` (auto-mutes during games/videos) | `true` |
| **Animation** | `transition_style` | `Slide`, `FloatAndFade`, `Instant` | `Slide` |
| **Animation** | `open_duration_ms` | `50..500` ms | `160` ms |
| **Animation** | `close_duration_ms` | `50..500` ms | `130` ms |
| **Focus & Lifecycle** | `auto_focus_on_open` | `true` / `false` | `true` |
| **Focus & Lifecycle** | `hide_on_mouse_leave` | `true` / `false` | `true` |
| **Focus & Lifecycle** | `mouse_leave_delay_ms` | `0..5000` ms | `350` ms |
| **Focus & Lifecycle** | `hide_on_focus_loss` | `true` / `false` (clicks outside window) | `true` |
| **Focus & Lifecycle** | `restore_prev_focus` | `true` / `false` | `true` |
| **Focus & Lifecycle** | `focus_restore_delay_ms`| `0..2000` ms | `50` ms |
| **Workspace** | `dock_mode` | `FloatingOverlay`, `NativeWorkAreaDock` | `FloatingOverlay` |
| **Workspace** | `virtual_desktop_mode` | `AllDesktops`, `CurrentDesktopOnly` | `AllDesktops` |
| **Visuals** | `edge_pill_style` | `MinimalLine`, `AppIconsStack`, `Hidden` | `AppIconsStack` |
| **Visuals** | `dim_background` | `0%..60%` backdrop scrim when drawer opens | `0%` |

---

## 6. Critical Pitfalls & Anti-Patterns (What NOT to Do)

### 1. NEVER Use Global Low-Level Mouse Hooks (`WH_MOUSE_LL`) for Edge Detection
* **Why:** A `WH_MOUSE_LL` hook forces the Windows kernel to context-switch into your process on **every single mouse movement event** across the entire OS. If someone uses an 8000Hz gaming mouse, your app will spike CPU usage and introduce mouse input lag in games.
* **What to Do Instead:** Create a `1px`-wide transparent, topmost window (`WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`) at the screen edge (or `8x8px` corner window for the Windows 8 Charms gesture). Your process sleeps at `0.00%` CPU until the OS cursor physically crosses into that window and sends `WM_MOUSEMOVE`.

### 2. NEVER Minimize (`SW_MINIMIZE`) or Hide (`SW_HIDE`) Stashed Windows
* **Why:**
  * `SW_MINIMIZE` causes Chromium, Electron (Spotify, Slack, VS Code, Obsidian), and WinUI apps to discard their DirectComposition visual surfaces. When you slide them back out, they render a blank white/black rectangle for 150ms.
  * `SW_HIDE` causes many apps (like Spotify or Discord) to think they have been closed to the system tray, breaking their internal window state!
* **What to Do Instead:** Move the window off-screen via `SetWindowPos` and apply `DwmSetWindowAttribute(..., DWMWA_CLOAK, &TRUE, ...)`. The application believes it is still normal and visible, keeping its GPU buffer 100% warm while DWM skips compositing it and prevents it from appearing on adjacent monitors.

### 3. NEVER Leave Users' Windows Stranded Off-Screen if Your App Crashes or Exits
* **Why:** If you move a user's browser off-screen (`X = 3840`) and cloak it (`DWMWA_CLOAK`), and the user kills your app in Task Manager or your app panics, their window is permanently invisible and unreachable until they reboot!
* **What to Do Instead:**
  1. Write every stashed `HWND` and its original `RECT` + `GWL_EXSTYLE` to `%LOCALAPPDATA%\EdgeStash\active_stashes.json` the instant a window is stashed.
  2. Register a Rust panic hook (`std::panic::set_hook`), a `CTRL_CLOSE_EVENT` handler, and a `WM_ENDSESSION` listener that uncloaks and moves all stashed `HWND`s back to the center of the primary monitor on exit.
  3. On startup, check `active_stashes.json`—if any leftover `HWND`s from an unclean shutdown are still valid and cloaked, immediately uncloak and restore them!

### 4. NEVER Resize the Target Window During the Slide Animation
* **Why:** Passing new `cx, cy` dimensions to `SetWindowPos` on every animation frame sends `WM_SIZE` and `WM_PAINT` to the target app 120 times per second, causing massive CPU spikes and jittery reflows in web browsers.
* **What to Do Instead:** Resize the window **once** when it is initially stashed (or while it is still cloaked before the animation starts), and always pass `SWP_NOSIZE` during the `DwmFlush()` animation loop so the GPU only translates an existing texture.

### 5. NEVER Trigger Edge Drawers When a Fullscreen Game or Video is Active
* **Why:** Nothing ruins a user's experience faster than flicking their mouse right in an FPS game or fullscreen YouTube video and accidentally sliding out their email client.
* **What to Do Instead:** Use `SHQueryUserNotificationState()` (`QUNS_BUSY`, `QUNS_RUNNING_D3D_FULL_SCREEN`, `QUNS_PRESENTATION_MODE`) or compare the foreground window's `GetWindowRect` to `GetMonitorInfoW().rcMonitor`. If the foreground window covers 100% of the monitor without `WS_CAPTION`, automatically disable edge triggers until they exit fullscreen.