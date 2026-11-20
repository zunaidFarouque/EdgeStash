use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::{GUID, Result};

/// Official documented COM wrapper over `IVirtualDesktopManager` (Windows 10/11 build-stable).
pub struct VirtualDesktopController {
    vdm: IVirtualDesktopManager,
    helper_hwnd: HWND,
}

impl VirtualDesktopController {
    pub fn new(helper_hwnd: HWND) -> Result<Self> {
        unsafe {
            let vdm: IVirtualDesktopManager =
                CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)?;
            Ok(Self { vdm, helper_hwnd })
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
        unsafe {
            self.vdm
                .GetWindowDesktopId(hwnd)
                .ok()
                .filter(|g| *g != GUID::zeroed())
        }
    }

    /// Queries the active Virtual Desktop GUID by inspecting the current foreground window
    /// or our helper window on the active desktop.
    pub fn get_current_desktop_id(&self) -> Option<GUID> {
        unsafe {
            let fg = GetForegroundWindow();
            if !fg.0.is_null()
                && self.is_on_current_desktop(fg)
                && let Some(id) = self.get_window_desktop_id(fg)
            {
                return Some(id);
            }
            if !self.helper_hwnd.0.is_null()
                && let Some(id) = self.get_window_desktop_id(self.helper_hwnd)
            {
                return Some(id);
            }
            None
        }
    }

    /// Pulls a globally-pinned stashed window onto the user's current active Virtual Desktop
    /// right before uncloaking and animating it.
    pub fn move_window_to_desktop(&self, hwnd: HWND, desktop_id: &GUID) -> Result<()> {
        unsafe { self.vdm.MoveWindowToDesktop(hwnd, desktop_id) }
    }
}
