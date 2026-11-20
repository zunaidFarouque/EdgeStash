use std::collections::HashMap;
use std::path::PathBuf;

use edgestash_ipc::config::{ChurnZoneAction, EdgeStashConfig};
use edgestash_ipc::protocol::{
    DaemonStatusSnapshot, EdgeSlotSnapshot, IpcErrorCode, IpcRequest, IpcResponse,
    StashedWindowSnapshot,
};
use edgestash_ipc::types::{
    ChurnZoneId, DockMode, Rect, ScreenEdge, VirtualDesktopMode, VirtualDesktopScope,
};

use crate::crash_recovery::{CrashManifest, CrashRecoveryEntry};
use crate::focus::{DismissalAction, DismissalController};
use crate::geometry::{
    clamp_drawer_thickness, compute_offbezel_origin, compute_parked_rect,
    compute_slot_rects_on_monitor, drawer_header_height, resolve_target_monitor,
    virtual_desktop_bounds,
};
use crate::platform::{
    AppBarApi, Clock, MonitorInfo, PillOverlayEvent, UnstashTargetState, VirtualDesktopApi,
    VisualOverlayApi, WindowAccessError, WindowHandle, WindowSystem, WindowTransition,
};

fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Structured engine error carrying an IPC error code and human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(pub IpcErrorCode, pub String);

impl EngineError {
    pub fn to_ipc_response(&self) -> IpcResponse {
        IpcResponse::Error {
            code: self.0,
            message: self.1.clone(),
        }
    }
}

/// Metadata for a single stashed window inside a [`DrawerSlot`].
#[derive(Debug, Clone)]
pub struct StashedWindow {
    pub hwnd: WindowHandle,
    pub title: String,
    pub process_name: String,
    pub original_ex_style: isize,
    pub original_rect: Rect,
    pub thickness_px: i32,
    pub target_rect: Rect,
    pub needs_attention: bool,
    pub attention_time: Option<u64>,
}

/// A single slot on a `(MonitorId, ScreenEdge)` deck. Holds 1 window normally or 2+ windows when grouped.
#[derive(Debug, Clone)]
pub struct DrawerSlot {
    pub monitor_id: String,
    pub edge: ScreenEdge,
    pub dock_mode: DockMode,
    pub preferred_pin_mode: DockMode,
    pub vd_scope: VirtualDesktopScope,
    pub is_open: bool,
    pub header_rect: Rect,
    pub docked_reserved_rect: Option<Rect>,
    pub windows: Vec<StashedWindow>,
}

impl DrawerSlot {
    pub fn max_thickness_px(&self) -> i32 {
        self.windows
            .iter()
            .map(|w| w.thickness_px)
            .max()
            .unwrap_or(320)
    }

    pub fn contains_hwnd(&self, hwnd: WindowHandle) -> bool {
        self.windows.iter().any(|w| w.hwnd == hwnd)
    }

    pub fn needs_attention(&self) -> bool {
        self.windows.iter().any(|w| w.needs_attention)
    }

    pub fn latest_attention_time(&self) -> Option<u64> {
        self.windows.iter().filter_map(|w| w.attention_time).max()
    }
}

#[derive(Debug, Clone, Default)]
struct EdgeDeck {
    slots: Vec<DrawerSlot>,
    active_index: usize,
}

/// Core stateful orchestrator driving stashing, grouping, animation transitions,
/// focus restoration, virtual desktop scoping, native AppBar docking, and crash persistence.
pub struct EdgeStashEngine<W, V, A, O> {
    windows: W,
    vdesktop: V,
    appbar: A,
    overlay: O,
    cfg: EdgeStashConfig,
    manifest_path: PathBuf,
    decks: HashMap<(String, ScreenEdge), EdgeDeck>,
    open_floating_edge: Option<(String, ScreenEdge, usize)>,
    last_default_opened_hwnd: Option<WindowHandle>,
    dismissal: DismissalController,
    pending_focus_restore: Option<(WindowHandle, u64)>,
    overlay_windows: Vec<WindowHandle>,
    drag_active: bool,
    config_path: Option<PathBuf>,
}

impl<W, V, A, O> EdgeStashEngine<W, V, A, O>
where
    W: WindowSystem,
    V: VirtualDesktopApi,
    A: AppBarApi,
    O: VisualOverlayApi,
{
    /// Creates a new engine instance with injected platform ports.
    pub fn new(
        windows: W,
        vdesktop: V,
        appbar: A,
        overlay: O,
        _clock: &dyn Clock,
        cfg: EdgeStashConfig,
        manifest_path: PathBuf,
    ) -> Self {
        Self {
            windows,
            vdesktop,
            appbar,
            overlay,
            cfg,
            manifest_path,
            decks: HashMap::new(),
            open_floating_edge: None,
            last_default_opened_hwnd: None,
            dismissal: DismissalController::default(),
            pending_focus_restore: None,
            overlay_windows: Vec::new(),
            drag_active: false,
            config_path: None,
        }
    }

    /// Sets the path to `config.toml` so internal config changes (such as corner churn zone drop bindings)
    /// immediately persist to disk.
    pub fn set_config_path(&mut self, path: PathBuf) {
        self.config_path = Some(path);
    }

    /// Updates active OLE drag-over-pill/drawer state so dismissal suspends until drop/release.
    pub fn set_drag_active(&mut self, active: bool) {
        self.drag_active = active;
        self.dismissal.set_drag_in_progress(active);
    }

    /// Returns the `28px` docked header strip rectangle for `(monitor_id, edge)` if open.
    pub fn header_rect_for_slot(&self, monitor_id: &str, edge: ScreenEdge) -> Option<Rect> {
        let key = (monitor_id.to_string(), edge);
        let idx = self.active_visible_slot_index(&key)?;
        let slot = self.decks.get(&key)?.slots.get(idx)?;
        slot.is_open.then_some(slot.header_rect)
    }

    /// Returns `(dock_mode, preferred_pin_mode, is_open)` for the active visible slot on `(monitor_id, edge)`.
    pub fn active_slot_modes(
        &self,
        monitor_id: &str,
        edge: ScreenEdge,
    ) -> Option<(DockMode, DockMode, bool)> {
        let key = (monitor_id.to_string(), edge);
        let idx = self.active_visible_slot_index(&key)?;
        let slot = self.decks.get(&key)?.slots.get(idx)?;
        Some((slot.dock_mode, slot.preferred_pin_mode, slot.is_open))
    }

    /// Registers internal daemon overlay windows so clicking/interacting with them does not dismiss drawers.
    pub fn set_overlay_windows(&mut self, hwnds: Vec<WindowHandle>) {
        self.overlay_windows = hwnds;
    }

    /// Updates live configuration (from TOML file watcher or IPC reload).
    pub fn update_config(&mut self, cfg: EdgeStashConfig) {
        self.cfg = cfg;
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &EdgeStashConfig {
        &self.cfg
    }

    /// Returns `true` if `(monitor_id, edge)` currently has at least one visible stashed slot.
    pub fn has_slots_on_edge(&self, monitor_id: &str, edge: ScreenEdge) -> bool {
        let key = (monitor_id.to_string(), edge);
        self.active_visible_slot_index(&key).is_some()
    }

    /// Returns `true` if any slot on `(monitor_id, edge)` is currently open.
    pub fn is_edge_open(&self, monitor_id: &str, edge: ScreenEdge) -> bool {
        let key = (monitor_id.to_string(), edge);
        self.decks
            .get(&key)
            .is_some_and(|d| d.slots.iter().any(|s| s.is_open))
    }

    /// Returns `true` if any slot across all monitors and edges is currently open.
    pub fn has_any_open_slot(&self) -> bool {
        self.decks
            .values()
            .any(|d| d.slots.iter().any(|s| s.is_open))
    }

    /// Returns `true` if a deferred focus restoration is pending execution.
    pub fn has_pending_focus_restore(&self) -> bool {
        self.pending_focus_restore.is_some()
    }

    /// Returns window handles for open floating (`FloatingOverlay` / `FloatingPinned`) drawers.
    pub fn open_floating_window_handles(&self) -> Vec<WindowHandle> {
        let mut out = Vec::new();
        for d in self.decks.values() {
            for s in &d.slots {
                if s.is_open
                    && matches!(
                        s.dock_mode,
                        DockMode::FloatingOverlay | DockMode::FloatingPinned
                    )
                {
                    for w in &s.windows {
                        out.push(w.hwnd);
                    }
                }
            }
        }
        out
    }

    /// Returns raw HWNDs of all currently open stashed windows (used to filter `EVENT_OBJECT_LOCATIONCHANGE`).
    pub fn open_stashed_hwnds(&self) -> Vec<isize> {
        let mut out = Vec::new();
        for d in self.decks.values() {
            for s in &d.slots {
                if s.is_open {
                    for w in &s.windows {
                        out.push(w.hwnd.0);
                    }
                }
            }
        }
        out
    }

    /// Stashes the current OS foreground window onto `edge`.
    pub fn stash_foreground(
        &mut self,
        edge: ScreenEdge,
        group_with_active: bool,
    ) -> Result<WindowHandle, EngineError> {
        let fg = self.windows.get_foreground_window().ok_or_else(|| {
            EngineError(
                IpcErrorCode::InvalidWindow,
                "No foreground window found".to_string(),
            )
        })?;
        self.stash_window(fg, edge, group_with_active)?;
        Ok(fg)
    }

    /// Captures and stashes `hwnd` onto `edge` (optionally grouping it with the active slot on that edge).
    pub fn stash_window(
        &mut self,
        hwnd: WindowHandle,
        edge: ScreenEdge,
        group_with_active: bool,
    ) -> Result<(), EngineError> {
        if hwnd.is_null() || self.overlay_windows.contains(&hwnd) {
            return Err(EngineError(
                IpcErrorCode::InvalidWindow,
                "Invalid or internal EdgeStash window".to_string(),
            ));
        }
        if self.find_window_slot(hwnd).is_some() {
            return Err(EngineError(
                IpcErrorCode::AlreadyStashed,
                "Window is already stashed".to_string(),
            ));
        }

        let snap = self.windows.snapshot_window(hwnd).map_err(map_access_err)?;
        let monitors = self.windows.enumerate_monitors();
        let monitor = select_monitor_for_rect(&monitors, snap.window_rect).ok_or_else(|| {
            EngineError(
                IpcErrorCode::InternalError,
                "No active monitor available".to_string(),
            )
        })?;
        let monitor_id = monitor.id.clone();

        let requested_thickness = if edge.is_vertical_edge() {
            snap.extended_frame_rect.width()
        } else {
            snap.extended_frame_rect.height()
        };
        let thickness_px = clamp_drawer_thickness(monitor, edge, requested_thickness);

        self.windows
            .prepare_window_for_stash(hwnd, snap.ex_style)
            .map_err(map_access_err)?;

        let stashed_win = StashedWindow {
            hwnd,
            title: snap.title,
            process_name: snap.process_name,
            original_ex_style: snap.ex_style,
            original_rect: snap.window_rect,
            thickness_px,
            target_rect: Rect::default(),
            needs_attention: false,
            attention_time: None,
        };

        let vd_scope = match self.cfg.virtual_desktop_mode {
            VirtualDesktopMode::AllDesktops => VirtualDesktopScope::AllDesktops,
            VirtualDesktopMode::CurrentDesktopOnly => self
                .vdesktop
                .get_window_desktop_id(hwnd)
                .or_else(|| self.vdesktop.get_current_desktop_id())
                .map(VirtualDesktopScope::from_guid)
                .unwrap_or(VirtualDesktopScope::AllDesktops),
        };

        let key = (monitor_id.clone(), edge);
        let deck = self.decks.entry(key.clone()).or_default();

        if group_with_active && !deck.slots.is_empty() {
            let idx = deck.active_index.min(deck.slots.len() - 1);
            deck.slots[idx].windows.push(stashed_win);
        } else {
            deck.slots.push(DrawerSlot {
                monitor_id: monitor_id.clone(),
                edge,
                dock_mode: self.cfg.dock_mode,
                preferred_pin_mode: if self.cfg.dock_mode == DockMode::NativeWorkAreaDock {
                    DockMode::NativeWorkAreaDock
                } else {
                    DockMode::FloatingPinned
                },
                vd_scope,
                is_open: false,
                header_rect: Rect::default(),
                docked_reserved_rect: None,
                windows: vec![stashed_win],
            });
            deck.active_index = deck.slots.len() - 1;
        }

        let active_idx = deck.active_index;
        let is_slot_open = deck.slots[active_idx].is_open;
        let slot_dock_mode = deck.slots[active_idx].dock_mode;
        self.recompute_slot_geometry(&key, active_idx);

        // Apply sized rect once before parking & cloaking (Pitfall #4: zero WM_SIZE during animation)
        let target_rect = self.decks[&key].slots[active_idx]
            .windows
            .iter()
            .find(|w| w.hwnd == hwnd)
            .expect("just inserted")
            .target_rect;
        self.windows.set_window_rect_visible(hwnd, target_rect);

        if is_slot_open {
            // Re-tile all visible windows in the open grouped slot and update header bar
            for w in &self.decks[&key].slots[active_idx].windows {
                self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
            }
            self.emit_header_for_slot(&key, active_idx, None);
        } else {
            let vbounds = virtual_desktop_bounds(&monitors);
            let parked = compute_parked_rect(vbounds, target_rect);
            self.windows.park_and_cloak_window(hwnd, parked);
        }

        if matches!(
            slot_dock_mode,
            DockMode::FloatingPinned | DockMode::NativeWorkAreaDock
        ) && !is_slot_open
        {
            let _ = self.open_slot(&key, active_idx);
        }

        self.last_default_opened_hwnd = Some(hwnd);
        self.sync_overlay_for_edge(&key);
        self.persist_manifest();
        Ok(())
    }

    /// Captures and stashes `hwnd` into a dedicated corner drop zone, binding that corner's
    /// churn trigger to this specific application.
    pub fn stash_window_and_bind_churn_zone(
        &mut self,
        hwnd: WindowHandle,
        zone: ChurnZoneId,
    ) -> Result<(), EngineError> {
        let snap = self.windows.snapshot_window(hwnd).map_err(map_access_err)?;
        let edge = zone.edge();
        self.stash_window(hwnd, edge, false)?;
        if !snap.process_name.is_empty() {
            self.cfg.churn_zones.set(
                zone,
                ChurnZoneAction::SpecificApp {
                    matcher: snap.process_name,
                },
            );
            if let Some(cfg_path) = &self.config_path {
                let _ = self.cfg.save_to_at(cfg_path);
            }
            self.persist_manifest();
        }
        Ok(())
    }

    /// Re-applies taskbar tab removal across all stashed windows (e.g. after Windows Explorer restarts).
    pub fn reapply_taskbar_deletion(&self) {
        let all_hwnds: Vec<WindowHandle> = self
            .decks
            .values()
            .flat_map(|d| d.slots.iter())
            .flat_map(|s| s.windows.iter().map(|w| w.hwnd))
            .collect();
        for hwnd in all_hwnds {
            self.windows.hide_from_taskbar(hwnd);
        }
    }

    /// Permanently un-stashes `hwnd`, restoring its original rectangle, style, and taskbar entry.
    pub fn unstash_window(&mut self, hwnd: WindowHandle) -> Result<(), EngineError> {
        self.unstash_window_with_state(hwnd, UnstashTargetState::Normal)
    }

    /// Permanently un-stashes `hwnd` with a specified post-unstash state (`Normal`, `Minimize`, `Maximize`).
    pub fn unstash_window_with_state(
        &mut self,
        hwnd: WindowHandle,
        target_state: UnstashTargetState,
    ) -> Result<(), EngineError> {
        let (key, slot_idx, win_idx) = self.find_window_slot(hwnd).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NotStashed,
                "Window is not stashed".to_string(),
            )
        })?;

        let (removed, prev_thickness) = {
            let deck = self.decks.get_mut(&key).expect("deck exists");
            let prev = deck.slots[slot_idx].max_thickness_px();
            (deck.slots[slot_idx].windows.remove(win_idx), prev)
        };
        if self.last_default_opened_hwnd == Some(removed.hwnd) {
            self.last_default_opened_hwnd = None;
        }

        self.windows.restore_window_from_stash_with_state(
            removed.hwnd,
            removed.original_ex_style,
            removed.original_rect,
            target_state,
        );

        self.cleanup_after_window_removal(&key, slot_idx, prev_thickness);
        self.persist_manifest();
        Ok(())
    }

    /// Un-stashes all windows in the active slot on `(monitor_id, edge)`.
    pub fn unstash_active(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
    ) -> Result<(), EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let active_idx = self.active_visible_slot_index(&key).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No stashed slot on edge for current desktop".to_string(),
            )
        })?;
        let hwnds: Vec<WindowHandle> = self.decks[&key].slots[active_idx]
            .windows
            .iter()
            .map(|w| w.hwnd)
            .collect();
        for hwnd in hwnds {
            self.unstash_window(hwnd)?;
        }
        Ok(())
    }

    /// Un-stashes every managed window cleanly (used on shutdown / explicit command).
    pub fn unstash_all(&mut self) {
        let all_hwnds: Vec<WindowHandle> = self
            .decks
            .values()
            .flat_map(|d| d.slots.iter())
            .flat_map(|s| s.windows.iter().map(|w| w.hwnd))
            .collect();
        for hwnd in all_hwnds {
            let _ = self.unstash_window(hwnd);
        }
        self.hide_dim_scrim();
    }

    /// Splits `hwnd` out of a multi-window group into its own standalone slot on the same edge.
    pub fn ungroup_window(&mut self, hwnd: WindowHandle) -> Result<(), EngineError> {
        let (key, slot_idx, win_idx) = self.find_window_slot(hwnd).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NotStashed,
                "Window is not stashed".to_string(),
            )
        })?;

        let slot_len = self.decks[&key].slots[slot_idx].windows.len();
        if slot_len <= 1 {
            return Ok(());
        }

        let extracted = self.decks.get_mut(&key).unwrap().slots[slot_idx]
            .windows
            .remove(win_idx);
        let parent_scope = self.decks[&key].slots[slot_idx].vd_scope;
        let was_open = self.decks[&key].slots[slot_idx].is_open;

        self.recompute_slot_geometry(&key, slot_idx);
        if was_open {
            for w in &self.decks[&key].slots[slot_idx].windows {
                self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
            }
        }

        let monitors = self.windows.enumerate_monitors();
        let vbounds = virtual_desktop_bounds(&monitors);

        let deck = self.decks.get_mut(&key).unwrap();
        deck.slots.push(DrawerSlot {
            monitor_id: key.0.clone(),
            edge: key.1,
            dock_mode: DockMode::FloatingOverlay,
            preferred_pin_mode: DockMode::FloatingPinned,
            vd_scope: parent_scope,
            is_open: false,
            header_rect: Rect::default(),
            docked_reserved_rect: None,
            windows: vec![extracted],
        });
        let new_idx = deck.slots.len() - 1;
        self.recompute_slot_geometry(&key, new_idx);

        let target_rect = self.decks[&key].slots[new_idx].windows[0].target_rect;
        self.windows.set_window_rect_visible(hwnd, target_rect);
        if was_open {
            let parked = compute_parked_rect(vbounds, target_rect);
            self.windows.park_and_cloak_window(hwnd, parked);
        }

        self.sync_overlay_for_edge(&key);
        Ok(())
    }

    /// Handles a mouse/corner trigger on `(monitor_id, edge)`. Respects `disable_in_fullscreen`.
    pub fn on_edge_trigger(&mut self, monitor_id: &str, edge: ScreenEdge) -> bool {
        if self.cfg.disable_in_fullscreen && self.windows.is_fullscreen_active() {
            return false;
        }
        let key = (monitor_id.to_string(), edge);
        let Some(idx) = self.active_visible_slot_index(&key) else {
            return false;
        };
        if self.decks[&key].slots[idx].is_open {
            return true;
        }
        self.open_slot_with_focus(&key, idx, true).is_ok()
    }

    fn resolve_churn_zone_target(
        &self,
        monitor_id: &str,
        zone: ChurnZoneId,
    ) -> Option<((String, ScreenEdge), usize, bool)> {
        let edge = zone.edge();
        match self.cfg.churn_zones.get(zone) {
            ChurnZoneAction::Disabled => None,
            ChurnZoneAction::SpecificApp { matcher } => {
                let needle = matcher.trim().to_ascii_lowercase();
                if needle.is_empty() {
                    return None;
                }
                let matches_slot = |slot: &DrawerSlot| -> bool {
                    slot.windows.iter().any(|w| {
                        w.process_name.to_ascii_lowercase().contains(&needle)
                            || w.title.to_ascii_lowercase().contains(&needle)
                    })
                };
                // Search triggered (monitor_id, edge) first, then other edges on monitor, then any monitor
                let mut search_keys: Vec<(String, ScreenEdge)> =
                    vec![(monitor_id.to_string(), edge)];
                for k in self.decks.keys() {
                    if k.0 == monitor_id && k.1 != edge {
                        search_keys.push(k.clone());
                    }
                }
                for k in self.decks.keys() {
                    if k.0 != monitor_id {
                        search_keys.push(k.clone());
                    }
                }
                for key in search_keys {
                    let vis = self.visible_slot_indices(&key);
                    for idx in vis {
                        if let Some(slot) = self.decks.get(&key).and_then(|d| d.slots.get(idx))
                            && matches_slot(slot)
                        {
                            return Some((key, idx, false));
                        }
                    }
                }
                None
            }
            ChurnZoneAction::DefaultLastOpened => {
                if let Some(def_hwnd) = self.last_default_opened_hwnd
                    && let Some((def_key, def_idx, _)) = self.find_window_slot(def_hwnd)
                    && self.visible_slot_indices(&def_key).contains(&def_idx)
                    && def_key == (monitor_id.to_string(), edge)
                {
                    return Some((def_key, def_idx, true));
                }
                let primary_key = (monitor_id.to_string(), edge);
                if let Some(idx) = self.active_visible_slot_index(&primary_key) {
                    return Some((primary_key, idx, true));
                }
                if let Some(def_hwnd) = self.last_default_opened_hwnd
                    && let Some((def_key, def_idx, _)) = self.find_window_slot(def_hwnd)
                    && self.visible_slot_indices(&def_key).contains(&def_idx)
                {
                    return Some((def_key, def_idx, true));
                }
                None
            }
        }
    }

    /// Returns `true` when `zone` on `monitor_id` currently has an assignable stashed target.
    pub fn has_churn_target(&self, monitor_id: &str, zone: ChurnZoneId) -> bool {
        self.resolve_churn_zone_target(monitor_id, zone).is_some()
    }

    /// Handles a directed corner-slide churn gesture according to `[churn_zones]` bindings.
    /// Returns `Some((opened_monitor_id, opened_edge))` when a target drawer opened (or was already open).
    pub fn on_churn_zone_trigger(
        &mut self,
        monitor_id: &str,
        zone: ChurnZoneId,
    ) -> Option<(String, ScreenEdge)> {
        if self.cfg.disable_in_fullscreen && self.windows.is_fullscreen_active() {
            return None;
        }
        let (key, idx, mark_as_default) = self.resolve_churn_zone_target(monitor_id, zone)?;
        if mark_as_default && let Some(deck) = self.decks.get_mut(&key) {
            deck.active_index = idx;
        }
        if self.decks[&key].slots[idx].is_open {
            self.sync_overlay_for_edge(&key);
            return Some(key);
        }
        if self
            .open_slot_with_focus_internal(&key, idx, true, mark_as_default)
            .is_ok()
        {
            Some(key)
        } else {
            None
        }
    }

    /// Opens the active slot when an OLE drag hovers onto the edge pill without stealing foreground focus mid-drag.
    pub fn on_drag_over_pill_trigger(&mut self, monitor_id: &str, edge: ScreenEdge) -> bool {
        if self.cfg.disable_in_fullscreen && self.windows.is_fullscreen_active() {
            return false;
        }
        let key = (monitor_id.to_string(), edge);
        let Some(idx) = self.active_visible_slot_index(&key) else {
            return false;
        };
        self.dismissal.mark_pointer_entered();
        if self.decks[&key].slots[idx].is_open {
            return true;
        }
        if self.open_slot_with_focus(&key, idx, false).is_ok() {
            self.dismissal.mark_pointer_entered();
            true
        } else {
            false
        }
    }

    /// Directly selects and opens `visible_slot_pos` (used by `AppIconsStack` individual icon clicks).
    pub fn select_and_open_slot(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
        visible_slot_pos: usize,
        toggle_if_already_active_open: bool,
    ) -> Result<(), EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let visible_indices = self.visible_slot_indices(&key);
        let Some(&target_idx) = visible_indices.get(visible_slot_pos) else {
            return Err(EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "Requested slot index not available on edge".to_string(),
            ));
        };

        let cur_active = self.decks[&key].active_index;
        let cur_open = self.decks[&key].slots[target_idx].is_open;
        if target_idx == cur_active && cur_open {
            if toggle_if_already_active_open {
                self.close_slot(&key, target_idx);
            }
            return Ok(());
        }

        if let Some(open_idx) = self.decks[&key].slots.iter().position(|s| s.is_open)
            && open_idx != target_idx
        {
            self.close_slot(&key, open_idx);
        }
        self.decks.get_mut(&key).unwrap().active_index = target_idx;
        if !self.decks[&key].slots[target_idx].is_open {
            self.open_slot(&key, target_idx)?;
        } else {
            self.sync_overlay_for_edge(&key);
        }
        Ok(())
    }

    /// Collapses the active open drawer back into its screen edge stash (header `[Close]` left-click).
    pub fn close_slot_drawer(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
    ) -> Result<(), EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let open_idx = self
            .decks
            .get(&key)
            .and_then(|d| d.slots.iter().position(|s| s.is_open))
            .or_else(|| self.active_visible_slot_index(&key))
            .ok_or_else(|| {
                EngineError(
                    IpcErrorCode::NoSlotOnEdge,
                    "No visible slot on edge".to_string(),
                )
            })?;
        self.close_slot(&key, open_idx);
        Ok(())
    }

    /// Sends OS close (`WM_CLOSE`) to the windows inside the active slot (header `[Close]` right/Shift-click).
    pub fn close_slot_app(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
    ) -> Result<(), EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let idx = self.active_visible_slot_index(&key).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No visible slot on edge".to_string(),
            )
        })?;
        let hwnds: Vec<WindowHandle> = self.decks[&key].slots[idx]
            .windows
            .iter()
            .map(|w| w.hwnd)
            .collect();
        for hwnd in hwnds {
            self.windows.close_window(hwnd);
        }
        Ok(())
    }

    /// Toggles pin state on the active slot (header `[Pin]` left-click):
    /// `FloatingOverlay` <-> `preferred_pin_mode` (`FloatingPinned` by default).
    pub fn toggle_pin_active(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
    ) -> Result<DockMode, EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let idx = self.active_visible_slot_index(&key).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No visible slot on edge".to_string(),
            )
        })?;
        let slot = &self.decks[&key].slots[idx];
        let next_mode = match slot.dock_mode {
            DockMode::FloatingOverlay => slot.preferred_pin_mode,
            DockMode::FloatingPinned | DockMode::NativeWorkAreaDock => DockMode::FloatingOverlay,
        };
        self.set_dock_mode(Some(&key.0), key.1, next_mode)?;
        Ok(next_mode)
    }

    /// Cycles the active slot on `(monitor_id, edge)` AND opens it if it was collapsed (used by area-aware scroll).
    pub fn cycle_edge_and_open_if_closed(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
        delta: i32,
    ) -> Result<usize, EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let idx = self.cycle_edge(Some(&key.0), key.1, delta)?;
        if !self.decks[&key].slots[idx].is_open {
            self.open_slot(&key, idx)?;
        }
        Ok(idx)
    }

    /// Toggles the active drawer on `(monitor_id, edge)` open or closed.
    /// Returns `Ok(true)` if opened, `Ok(false)` if closed.
    pub fn toggle_edge(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
    ) -> Result<bool, EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let idx = self.active_visible_slot_index(&key).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No visible stashed slot on edge".to_string(),
            )
        })?;
        let is_open = self.decks[&key].slots[idx].is_open;
        if is_open {
            self.close_slot(&key, idx);
            Ok(false)
        } else {
            self.open_slot(&key, idx)?;
            Ok(true)
        }
    }

    /// Returns true if any floating overlay drawer is currently open on any monitor.
    pub fn has_open_floating_drawer(&self) -> bool {
        self.open_floating_edge.is_some()
    }

    /// Toggles whichever floating drawer is currently open (or the first available slot).
    pub fn toggle_active(&mut self) -> Result<bool, EngineError> {
        if let Some((mon, edge, idx)) = self.open_floating_edge.clone() {
            let key = (mon, edge);
            if self
                .decks
                .get(&key)
                .is_some_and(|d| d.slots.get(idx).is_some_and(|s| s.is_open))
            {
                self.close_slot(&key, idx);
                return Ok(false);
            }
        }
        // Find any visible slot on primary monitor or first deck
        let monitors = self.windows.enumerate_monitors();
        let preferred_mon = resolve_target_monitor(&monitors, None).map(|m| m.id.clone());
        let mut candidate: Option<((String, ScreenEdge), usize)> = None;
        for (key, deck) in &self.decks {
            if !deck.slots.is_empty()
                && let Some(idx) = self.active_visible_slot_index(key)
            {
                if Some(&key.0) == preferred_mon.as_ref() {
                    candidate = Some((key.clone(), idx));
                    break;
                }
                if candidate.is_none() {
                    candidate = Some((key.clone(), idx));
                }
            }
        }
        let (key, idx) = candidate.ok_or_else(|| {
            EngineError(IpcErrorCode::NoSlotOnEdge, "No stashed windows".to_string())
        })?;
        self.open_slot(&key, idx)?;
        Ok(true)
    }

    /// Cycles the active slot in the `(monitor_id, edge)` stack by `delta` (e.g. +1 or -1).
    pub fn cycle_edge(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
        delta: i32,
    ) -> Result<usize, EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let visible_indices = self.visible_slot_indices(&key);
        if visible_indices.is_empty() {
            return Err(EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No visible slots on edge".to_string(),
            ));
        }

        let current_active = self.decks[&key].active_index;
        let current_pos = visible_indices
            .iter()
            .position(|&i| i == current_active)
            .unwrap_or(0);
        let len = visible_indices.len() as isize;
        let next_pos = ((current_pos as isize + delta as isize).rem_euclid(len)) as usize;
        let next_idx = visible_indices[next_pos];

        if next_idx == current_active {
            return Ok(next_idx);
        }

        let currently_open_slot = self.decks[&key].slots.iter().position(|s| s.is_open);
        if let Some(open_idx) = currently_open_slot {
            self.close_slot(&key, open_idx);
        }
        self.decks.get_mut(&key).unwrap().active_index = next_idx;
        let primary_hwnd = self.decks[&key].slots[next_idx]
            .windows
            .first()
            .map(|w| w.hwnd);
        if let Some(h) = primary_hwnd {
            self.last_default_opened_hwnd = Some(h);
        }
        if currently_open_slot.is_some() {
            self.open_slot(&key, next_idx)?;
        } else {
            self.sync_overlay_for_edge(&key);
        }
        Ok(next_idx)
    }

    /// Switches `DockMode` (`FloatingOverlay` / `FloatingPinned` / `NativeWorkAreaDock`) for the active slot on `(monitor_id, edge)`.
    pub fn set_dock_mode(
        &mut self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
        mode: DockMode,
    ) -> Result<(), EngineError> {
        let key = self.resolve_deck_key(monitor_id, edge)?;
        let idx = self.active_visible_slot_index(&key).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No visible slot on edge".to_string(),
            )
        })?;

        let prev_mode = self.decks[&key].slots[idx].dock_mode;
        if prev_mode == mode {
            return Ok(());
        }

        let monitors = self.windows.enumerate_monitors();
        let monitor = resolve_target_monitor(&monitors, Some(&key.0))
            .cloned()
            .ok_or_else(|| {
                EngineError(
                    IpcErrorCode::InternalError,
                    "Monitor unavailable".to_string(),
                )
            })?;
        let thickness = self.decks[&key].slots[idx].max_thickness_px();

        if matches!(
            mode,
            DockMode::FloatingPinned | DockMode::NativeWorkAreaDock
        ) {
            self.decks.get_mut(&key).unwrap().slots[idx].preferred_pin_mode = mode;
        }

        if prev_mode == DockMode::NativeWorkAreaDock && mode != DockMode::NativeWorkAreaDock {
            self.appbar
                .register_or_update_dock(&key.0, key.1, monitor.rect, thickness, false);
            self.decks.get_mut(&key).unwrap().slots[idx].docked_reserved_rect = None;
        }

        match mode {
            DockMode::NativeWorkAreaDock => {
                let reserved = self.appbar.register_or_update_dock(
                    &key.0,
                    key.1,
                    monitor.rect,
                    thickness,
                    true,
                );
                {
                    let slot = &mut self.decks.get_mut(&key).unwrap().slots[idx];
                    slot.dock_mode = mode;
                    slot.docked_reserved_rect = reserved;
                }
                self.recompute_slot_geometry(&key, idx);
                if self.open_floating_edge.as_ref() == Some(&(key.0.clone(), key.1, idx)) {
                    self.open_floating_edge = None;
                    self.dismissal.reset();
                    self.hide_dim_scrim();
                }
                if !self.decks[&key].slots[idx].is_open {
                    self.open_slot(&key, idx)?;
                } else {
                    for w in &self.decks[&key].slots[idx].windows {
                        if (w.original_ex_style & 0x0000_0008) == 0 {
                            self.windows.set_window_topmost(w.hwnd, false);
                        }
                        self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
                    }
                    self.emit_header_for_slot(&key, idx, None);
                    self.sync_overlay_for_edge(&key);
                }
            }
            DockMode::FloatingPinned => {
                self.decks.get_mut(&key).unwrap().slots[idx].dock_mode = mode;
                self.recompute_slot_geometry(&key, idx);
                if self.open_floating_edge.as_ref() == Some(&(key.0.clone(), key.1, idx)) {
                    self.open_floating_edge = None;
                    self.dismissal.reset();
                    self.hide_dim_scrim();
                }
                if !self.decks[&key].slots[idx].is_open {
                    self.open_slot(&key, idx)?;
                } else {
                    for w in &self.decks[&key].slots[idx].windows {
                        self.windows.set_window_topmost(w.hwnd, true);
                        self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
                    }
                    self.emit_header_for_slot(&key, idx, None);
                    self.sync_overlay_for_edge(&key);
                }
            }
            DockMode::FloatingOverlay => {
                self.decks.get_mut(&key).unwrap().slots[idx].dock_mode = mode;
                self.recompute_slot_geometry(&key, idx);
                if self.decks[&key].slots[idx].is_open {
                    if let Some((open_mon, open_edge, open_idx)) = self.open_floating_edge.clone()
                        && (open_mon.clone(), open_edge, open_idx) != (key.0.clone(), key.1, idx)
                    {
                        self.close_slot(&(open_mon, open_edge), open_idx);
                    }
                    self.open_floating_edge = Some((key.0.clone(), key.1, idx));
                    self.dismissal
                        .on_drawer_opened(None, DockMode::FloatingOverlay);
                    if self.drag_active {
                        self.dismissal.set_drag_in_progress(true);
                    }
                    for w in &self.decks[&key].slots[idx].windows {
                        self.windows.set_window_topmost(w.hwnd, true);
                        self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
                    }
                    if self.cfg.dim_background > 0 {
                        self.overlay
                            .emit_overlay_event(PillOverlayEvent::SetDimScrim {
                                visible: true,
                                monitor_rect: monitor.rect,
                                opacity_percent: self.cfg.dim_background,
                            });
                    }
                    self.emit_header_for_slot(&key, idx, None);
                    self.sync_overlay_for_edge(&key);
                }
            }
        }
        Ok(())
    }

    /// Updates virtual desktop scope (`AllDesktops` or `CurrentDesktopOnly`) for the slot containing `hwnd`.
    pub fn set_virtual_desktop_mode(
        &mut self,
        hwnd: WindowHandle,
        mode: VirtualDesktopMode,
    ) -> Result<(), EngineError> {
        let (key, slot_idx, _) = self.find_window_slot(hwnd).ok_or_else(|| {
            EngineError(
                IpcErrorCode::NotStashed,
                "Window is not stashed".to_string(),
            )
        })?;
        let scope = match mode {
            VirtualDesktopMode::AllDesktops => VirtualDesktopScope::AllDesktops,
            VirtualDesktopMode::CurrentDesktopOnly => self
                .vdesktop
                .get_window_desktop_id(hwnd)
                .or_else(|| self.vdesktop.get_current_desktop_id())
                .map(VirtualDesktopScope::from_guid)
                .unwrap_or(VirtualDesktopScope::AllDesktops),
        };
        self.decks.get_mut(&key).unwrap().slots[slot_idx].vd_scope = scope;
        self.sync_overlay_for_edge(&key);
        Ok(())
    }

    /// Periodic tick (~30Hz while a floating drawer is open or focus restore is pending).
    pub fn tick(&mut self, clock: &dyn Clock) {
        let now_ms = clock.now_ms();

        if let Some((target, restore_at)) = self.pending_focus_restore
            && now_ms >= restore_at
        {
            self.pending_focus_restore = None;
            self.windows.force_restore_foreground_window(target);
        }

        let Some((mon_id, edge, slot_idx)) = self.open_floating_edge.clone() else {
            return;
        };
        let key = (mon_id, edge);
        let Some(slot) = self.decks.get(&key).and_then(|d| d.slots.get(slot_idx)) else {
            self.open_floating_edge = None;
            self.dismissal.reset();
            return;
        };
        if !slot.is_open || slot.dock_mode != DockMode::FloatingOverlay {
            return;
        }

        let Some(cursor) = self.windows.get_cursor_pos() else {
            return;
        };
        let mut group_rects: Vec<Rect> = slot
            .windows
            .iter()
            .map(|w| {
                self.windows
                    .get_window_rect(w.hwnd)
                    .unwrap_or(w.target_rect)
            })
            .collect();
        if slot.header_rect.width() > 0 && slot.header_rect.height() > 0 {
            group_rects.push(slot.header_rect);
        }
        let has_popup = slot
            .windows
            .iter()
            .any(|w| self.windows.has_active_child_popup(w.hwnd));

        match self
            .dismissal
            .tick(cursor, &group_rects, has_popup, now_ms, &self.cfg)
        {
            DismissalAction::None => {}
            DismissalAction::HideImmediate => {
                self.close_slot(&key, slot_idx);
            }
            DismissalAction::HideAndRestoreFocus {
                restore_target,
                delay_ms,
            } => {
                self.close_slot(&key, slot_idx);
                if let Some(target) = restore_target {
                    if delay_ms == 0 {
                        self.windows.force_restore_foreground_window(target);
                    } else {
                        self.pending_focus_restore = Some((target, now_ms + u64::from(delay_ms)));
                    }
                }
            }
        }
    }

    /// Sets timestamp for active drawer reveal to enforce opening transition grace period.
    pub fn mark_drawer_opened_at(&mut self, now_ms: u64) {
        self.dismissal.set_opened_at_ms(now_ms);
    }

    /// Handles `EVENT_SYSTEM_FOREGROUND` with timestamp and descendant window resolution.
    pub fn on_foreground_changed_at(&mut self, new_fg: WindowHandle, now_ms: u64) {
        let Some((mon_id, edge, slot_idx)) = self.open_floating_edge.clone() else {
            return;
        };
        let key = (mon_id, edge);
        let Some(slot) = self.decks.get(&key).and_then(|d| d.slots.get(slot_idx)) else {
            return;
        };
        let group_hwnds: Vec<WindowHandle> = slot.windows.iter().map(|w| w.hwnd).collect();
        let is_ours = group_hwnds
            .iter()
            .any(|&w| self.windows.is_window_or_descendant(new_fg, w))
            || self
                .overlay_windows
                .iter()
                .any(|&o| self.windows.is_window_or_descendant(new_fg, o));

        let cursor_over_drawer = self.windows.get_cursor_pos().is_some_and(|cursor| {
            let mut group_rects: Vec<Rect> = slot
                .windows
                .iter()
                .map(|w| {
                    self.windows
                        .get_window_rect(w.hwnd)
                        .unwrap_or(w.target_rect)
                })
                .collect();
            if slot.header_rect.width() > 0 && slot.header_rect.height() > 0 {
                group_rects.push(slot.header_rect);
            }
            group_rects
                .iter()
                .any(|r| r.contains_inclusive(cursor.0, cursor.1))
        });

        let target_hwnd = if is_ours {
            group_hwnds.first().copied().unwrap_or(new_fg)
        } else {
            new_fg
        };

        let action = self.dismissal.on_foreground_changed_at(
            target_hwnd,
            &group_hwnds,
            &self.overlay_windows,
            cursor_over_drawer,
            now_ms,
            &self.cfg,
        );
        if is_ours {
            let mut cleared_any = false;
            if let Some(slot_mut) = self
                .decks
                .get_mut(&key)
                .and_then(|d| d.slots.get_mut(slot_idx))
            {
                for win in &mut slot_mut.windows {
                    if win.needs_attention {
                        win.needs_attention = false;
                        win.attention_time = None;
                        cleared_any = true;
                    }
                }
            }
            if cleared_any {
                self.sync_overlay_for_edge(&key);
            }
        }

        if matches!(action, DismissalAction::HideImmediate) {
            self.close_slot(&key, slot_idx);
        }
    }

    /// Handles `EVENT_SYSTEM_FOREGROUND` to immediately dismiss floating drawers when clicking outside.
    pub fn on_foreground_changed(&mut self, new_fg: WindowHandle) {
        self.on_foreground_changed_at(new_fg, 0);
    }

    /// Enforces stashed-window edge anchoring during move events while preserving inner-edge thickness resizes.
    /// Returns `true` when window geometry was snapped or updated.
    pub fn on_window_moved_or_resized(&mut self, hwnd: WindowHandle, is_live_move: bool) -> bool {
        let Some((key, slot_idx, win_idx)) = self.find_window_slot(hwnd) else {
            return false;
        };
        if !self.decks[&key].slots[slot_idx].is_open {
            return false;
        }
        if self.windows.is_window_zoomed(hwnd) {
            let _ = self.unstash_window_with_state(hwnd, UnstashTargetState::Maximize);
            return true;
        }
        let Some(current_rect) = self.windows.get_window_rect(hwnd) else {
            return false;
        };
        let target_rect = self.decks[&key].slots[slot_idx].windows[win_idx].target_rect;
        if (current_rect.left - target_rect.left).abs() <= 1
            && (current_rect.top - target_rect.top).abs() <= 1
            && (current_rect.right - target_rect.right).abs() <= 1
            && (current_rect.bottom - target_rect.bottom).abs() <= 1
        {
            return false;
        }

        // Live move (EVENT_OBJECT_LOCATIONCHANGE while user is dragging the title bar):
        // NEVER snap back in real time! Doing so fights Windows' move loop and causes stutter/jitter.
        if is_live_move {
            return false;
        }

        let edge_distance = match key.1 {
            ScreenEdge::Left => (current_rect.left - target_rect.left).abs(),
            ScreenEdge::Right => (current_rect.right - target_rect.right).abs(),
            ScreenEdge::Top => (current_rect.top - target_rect.top).abs(),
            ScreenEdge::Bottom => (current_rect.bottom - target_rect.bottom).abs(),
        };

        let same_width = (current_rect.width() - target_rect.width()).abs() <= 1;
        let same_height = (current_rect.height() - target_rect.height()).abs() <= 1;

        // If lock_stashed_position is enabled and window was dragged far into the screen (> 100px from bezel):
        // Automatically un-stash the window onto the desktop where dropped!
        if self.cfg.lock_stashed_position && edge_distance > 100 {
            if let Some(deck) = self.decks.get_mut(&key)
                && let Some(slot) = deck.slots.get_mut(slot_idx)
                && let Some(w) = slot.windows.get_mut(win_idx)
            {
                w.original_rect = current_rect;
            }
            let _ = self.unstash_window(hwnd);
            return true;
        }

        let monitors = self.windows.enumerate_monitors();
        let Some(mon) = resolve_target_monitor(&monitors, Some(&key.0)) else {
            return false;
        };
        let raw_thickness = if key.1.is_vertical_edge() {
            current_rect.width()
        } else {
            current_rect.height() + drawer_header_height(mon.dpi)
        };
        let clamped = clamp_drawer_thickness(mon, key.1, raw_thickness);
        let prev_thickness = self.decks[&key].slots[slot_idx].windows[win_idx].thickness_px;
        let thickness_changed = clamped != prev_thickness;

        if !self.cfg.lock_stashed_position && same_width && same_height {
            return false;
        }

        if thickness_changed && edge_distance <= 2 {
            self.decks.get_mut(&key).unwrap().slots[slot_idx].windows[win_idx].thickness_px =
                clamped;
            if self.decks[&key].slots[slot_idx].dock_mode == DockMode::NativeWorkAreaDock {
                let max_thick = self.decks[&key].slots[slot_idx].max_thickness_px();
                let reserved = self
                    .appbar
                    .register_or_update_dock(&key.0, key.1, mon.rect, max_thick, true);
                self.decks.get_mut(&key).unwrap().slots[slot_idx].docked_reserved_rect = reserved;
            }
        }

        self.recompute_slot_geometry(&key, slot_idx);
        if self.cfg.lock_stashed_position || (thickness_changed && edge_distance <= 2) {
            for w in &self.decks[&key].slots[slot_idx].windows {
                self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
            }
            self.emit_header_for_slot(&key, slot_idx, None);
            return true;
        }
        false
    }

    /// Remembers manual thickness resizing performed while a drawer is open (and locks position when enabled).
    pub fn on_window_resized(&mut self, hwnd: WindowHandle) {
        let _ = self.on_window_moved_or_resized(hwnd, false);
    }

    /// Purges a stashed window that closed externally and triggers the pill's `WindowClosedAnimation`.
    pub fn on_window_destroyed(&mut self, hwnd: WindowHandle) {
        let Some((key, slot_idx, win_idx)) = self.find_window_slot(hwnd) else {
            return;
        };
        let (removed, prev_thickness) = {
            let deck = self.decks.get_mut(&key).unwrap();
            let prev = deck.slots[slot_idx].max_thickness_px();
            (deck.slots[slot_idx].windows.remove(win_idx), prev)
        };
        if self.last_default_opened_hwnd == Some(removed.hwnd) {
            self.last_default_opened_hwnd = None;
        }

        self.overlay
            .emit_overlay_event(PillOverlayEvent::WindowClosedAnimation {
                monitor_id: key.0.clone(),
                edge: key.1,
                closed_hwnd: removed.hwnd,
                closed_title: removed.title,
            });

        self.cleanup_after_window_removal(&key, slot_idx, prev_thickness);
        self.persist_manifest();
    }

    /// Handles `WM_DISPLAYCHANGE` by migrating slots from disconnected monitors to the primary monitor.
    pub fn on_monitors_changed(&mut self) {
        let monitors = self.windows.enumerate_monitors();
        let Some(primary) = resolve_target_monitor(&monitors, None).cloned() else {
            return;
        };
        let valid_ids: Vec<String> = monitors.iter().map(|m| m.id.clone()).collect();
        let orphaned_keys: Vec<(String, ScreenEdge)> = self
            .decks
            .keys()
            .filter(|(mon_id, _)| !valid_ids.contains(mon_id))
            .cloned()
            .collect();

        for old_key in orphaned_keys {
            if let Some(old_deck) = self.decks.remove(&old_key) {
                let target_key = (primary.id.clone(), old_key.1);
                let target_deck = self.decks.entry(target_key.clone()).or_default();
                for mut slot in old_deck.slots {
                    slot.monitor_id = primary.id.clone();
                    slot.is_open = false;
                    target_deck.slots.push(slot);
                }
                let len = target_deck.slots.len();
                for idx in 0..len {
                    self.recompute_slot_geometry(&target_key, idx);
                    let vbounds = virtual_desktop_bounds(&monitors);
                    for w in &self.decks[&target_key].slots[idx].windows {
                        self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
                        if !self.decks[&target_key].slots[idx].is_open {
                            let parked = compute_parked_rect(vbounds, w.target_rect);
                            self.windows.park_and_cloak_window(w.hwnd, parked);
                        }
                    }
                }
                self.sync_overlay_for_edge(&target_key);
            }
        }
        self.persist_manifest();
    }

    /// Produces a serializable snapshot of all active slots for IPC status queries.
    pub fn status_snapshot(&self, pid: u32) -> DaemonStatusSnapshot {
        let mut slots = Vec::new();
        for (key, deck) in &self.decks {
            for (idx, slot) in deck.slots.iter().enumerate() {
                slots.push(EdgeSlotSnapshot {
                    monitor_id: key.0.clone(),
                    edge: key.1,
                    slot_index: idx,
                    is_active_in_deck: idx == deck.active_index,
                    is_open: slot.is_open,
                    dock_mode: slot.dock_mode,
                    virtual_desktop_mode: match slot.vd_scope {
                        VirtualDesktopScope::AllDesktops => VirtualDesktopMode::AllDesktops,
                        VirtualDesktopScope::PinnedToDesktop(_) => {
                            VirtualDesktopMode::CurrentDesktopOnly
                        }
                    },
                    needs_attention: slot.needs_attention(),
                    windows: slot
                        .windows
                        .iter()
                        .map(|w| StashedWindowSnapshot {
                            hwnd: w.hwnd.0,
                            title: w.title.clone(),
                            process_name: w.process_name.clone(),
                            target_rect: w.target_rect,
                            needs_attention: w.needs_attention,
                        })
                        .collect(),
                });
            }
        }
        DaemonStatusSnapshot { pid, slots }
    }

    /// Marks a window as requesting attention (e.g. on Win32 HSHELL_FLASH / HSHELL_RUDEAPPACTIVATED).
    /// Returns `Some((monitor_id, edge, slot_idx))` if the window was found in a stashed deck.
    pub fn handle_window_attention(
        &mut self,
        hwnd: WindowHandle,
        now_ms: u64,
    ) -> Option<(String, ScreenEdge, usize)> {
        // Foreground windows being actively used by the user never request attention.
        if self.windows.get_foreground_window() == Some(hwnd) {
            return None;
        }
        let mut target = None;
        for (key, deck) in &mut self.decks {
            for (slot_idx, slot) in deck.slots.iter_mut().enumerate() {
                for win in &mut slot.windows {
                    if win.hwnd == hwnd {
                        win.needs_attention = true;
                        win.attention_time = Some(now_ms);
                        target = Some((key.0.clone(), key.1, slot_idx));
                        break;
                    }
                }
                if target.is_some() {
                    break;
                }
            }
            if target.is_some() {
                break;
            }
        }
        if let Some((ref mon, edge, _)) = target {
            self.sync_overlay_for_edge(&(mon.clone(), edge));
        }
        target
    }

    /// Simulates attention on a stashed window for testing and CLI.
    pub fn simulate_window_attention(
        &mut self,
        target_hwnd: Option<WindowHandle>,
        edge: Option<ScreenEdge>,
        now_ms: u64,
    ) -> Result<WindowHandle, EngineError> {
        if let Some(h) = target_hwnd {
            if self.handle_window_attention(h, now_ms).is_some() {
                return Ok(h);
            }
            return Err(EngineError(
                IpcErrorCode::NotStashed,
                format!("Window {} is not stashed", h.0),
            ));
        }

        let mut matched = None;
        for (key, deck) in &mut self.decks {
            if let Some(req_edge) = edge
                && key.1 != req_edge
            {
                continue;
            }
            for slot in &mut deck.slots {
                if let Some(win) = slot.windows.first_mut() {
                    let hwnd = win.hwnd;
                    win.needs_attention = true;
                    win.attention_time = Some(now_ms);
                    matched = Some((key.clone(), hwnd));
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }

        if let Some((key, hwnd)) = matched {
            self.sync_overlay_for_edge(&key);
            Ok(hwnd)
        } else {
            Err(EngineError(
                IpcErrorCode::NoSlotOnEdge,
                "No stashed window found to simulate attention".to_string(),
            ))
        }
    }

    /// Dispatches a single [`IpcRequest`] and produces an [`IpcResponse`].
    pub fn handle_ipc_request(&mut self, req: IpcRequest, pid: u32) -> IpcResponse {
        match req {
            IpcRequest::Ping => IpcResponse::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                pid,
            },
            IpcRequest::Status => IpcResponse::Status(self.status_snapshot(pid)),
            IpcRequest::StashForeground {
                edge,
                group_with_active,
            } => match self.stash_foreground(edge, group_with_active) {
                Ok(hwnd) => IpcResponse::Ok {
                    message: format!("Stashed window {}", hwnd.0),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::StashWindow {
                hwnd,
                edge,
                group_with_active,
            } => match self.stash_window(WindowHandle(hwnd), edge, group_with_active) {
                Ok(()) => IpcResponse::Ok {
                    message: format!("Stashed window {hwnd}"),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::UnstashWindow { hwnd } => match self.unstash_window(WindowHandle(hwnd)) {
                Ok(()) => IpcResponse::Ok {
                    message: format!("Unstashed window {hwnd}"),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::UnstashActive { monitor_id, edge } => {
                match self.unstash_active(monitor_id.as_deref(), edge) {
                    Ok(()) => IpcResponse::Ok {
                        message: format!("Unstashed active slot on {edge:?}"),
                    },
                    Err(e) => e.to_ipc_response(),
                }
            }
            IpcRequest::UnstashAll => {
                self.unstash_all();
                IpcResponse::Ok {
                    message: "Unstashed all windows".to_string(),
                }
            }
            IpcRequest::ToggleEdge { monitor_id, edge } => {
                match self.toggle_edge(monitor_id.as_deref(), edge) {
                    Ok(opened) => IpcResponse::Ok {
                        message: if opened { "Opened" } else { "Closed" }.to_string(),
                    },
                    Err(e) => e.to_ipc_response(),
                }
            }
            IpcRequest::ToggleActive => match self.toggle_active() {
                Ok(opened) => IpcResponse::Ok {
                    message: if opened { "Opened" } else { "Closed" }.to_string(),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::CycleEdge {
                monitor_id,
                edge,
                delta,
            } => match self.cycle_edge(monitor_id.as_deref(), edge, delta) {
                Ok(idx) => IpcResponse::Ok {
                    message: format!("Active slot index {idx}"),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::SetDockMode {
                monitor_id,
                edge,
                mode,
            } => match self.set_dock_mode(monitor_id.as_deref(), edge, mode) {
                Ok(()) => IpcResponse::Ok {
                    message: format!("Dock mode set to {mode:?}"),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::SetVirtualDesktopMode { hwnd, mode } => {
                match self.set_virtual_desktop_mode(WindowHandle(hwnd), mode) {
                    Ok(()) => IpcResponse::Ok {
                        message: format!("Virtual desktop mode set to {mode:?}"),
                    },
                    Err(e) => e.to_ipc_response(),
                }
            }
            IpcRequest::UngroupWindow { hwnd } => match self.ungroup_window(WindowHandle(hwnd)) {
                Ok(()) => IpcResponse::Ok {
                    message: format!("Ungrouped window {hwnd}"),
                },
                Err(e) => e.to_ipc_response(),
            },
            IpcRequest::SimulateWindowAttention { hwnd, edge } => {
                let target = hwnd.map(WindowHandle);
                let now_ms = current_epoch_ms();
                match self.simulate_window_attention(target, edge, now_ms) {
                    Ok(h) => IpcResponse::Ok {
                        message: format!("Simulated attention for window {}", h.0),
                    },
                    Err(e) => e.to_ipc_response(),
                }
            }
            IpcRequest::ReloadConfig
            | IpcRequest::Shutdown
            | IpcRequest::PreviewChurnVisual { .. } => IpcResponse::Ok {
                message: "Acknowledged".to_string(),
            },
        }
    }

    fn open_slot(
        &mut self,
        key: &(String, ScreenEdge),
        slot_idx: usize,
    ) -> Result<(), EngineError> {
        self.open_slot_with_focus(key, slot_idx, true)
    }

    fn open_slot_with_focus(
        &mut self,
        key: &(String, ScreenEdge),
        slot_idx: usize,
        allow_auto_focus: bool,
    ) -> Result<(), EngineError> {
        self.open_slot_with_focus_internal(key, slot_idx, allow_auto_focus, true)
    }

    fn open_slot_with_focus_internal(
        &mut self,
        key: &(String, ScreenEdge),
        slot_idx: usize,
        allow_auto_focus: bool,
        mark_as_default: bool,
    ) -> Result<(), EngineError> {
        // Close any other open unpinned floating drawer globally
        if let Some((open_mon, open_edge, open_idx)) = self.open_floating_edge.clone()
            && (open_mon.clone(), open_edge, open_idx) != (key.0.clone(), key.1, slot_idx)
        {
            self.close_slot(&(open_mon, open_edge), open_idx);
        }
        // Close any other open slot on this same (monitor, edge) deck
        if let Some(prev_same_open) = self
            .decks
            .get(key)
            .and_then(|d| d.slots.iter().position(|s| s.is_open))
            && prev_same_open != slot_idx
        {
            self.close_slot(key, prev_same_open);
        }

        let monitors = self.windows.enumerate_monitors();
        let monitor = resolve_target_monitor(&monitors, Some(&key.0))
            .cloned()
            .ok_or_else(|| {
                EngineError(
                    IpcErrorCode::InternalError,
                    "Monitor unavailable".to_string(),
                )
            })?;

        if let Some(slot_mut) = self
            .decks
            .get_mut(key)
            .and_then(|d| d.slots.get_mut(slot_idx))
            && slot_mut.dock_mode == DockMode::NativeWorkAreaDock
            && slot_mut.docked_reserved_rect.is_none()
        {
            let thick = slot_mut.max_thickness_px();
            slot_mut.docked_reserved_rect =
                self.appbar
                    .register_or_update_dock(&key.0, key.1, monitor.rect, thick, true);
        }

        self.recompute_slot_geometry(key, slot_idx);

        let slot = &self.decks[key].slots[slot_idx];
        let dock_mode = slot.dock_mode;
        let vd_scope = slot.vd_scope;
        let target_header_rect = slot.header_rect;

        if vd_scope == VirtualDesktopScope::AllDesktops
            && let Some(current_id) = self.vdesktop.get_current_desktop_id()
        {
            for w in &slot.windows {
                let _ = self.vdesktop.move_window_to_desktop(w.hwnd, &current_id);
            }
        }

        let previous_fg = self.windows.get_foreground_window().filter(|&fg| {
            !self.overlay_windows.contains(&fg) && !slot.windows.iter().any(|w| w.hwnd == fg)
        });
        let mut transitions: Vec<WindowTransition> = slot
            .windows
            .iter()
            .map(|w| {
                self.windows.ensure_window_restored(w.hwnd);
                let off_bezel = compute_offbezel_origin(&monitor, key.1, w.target_rect);
                self.windows.set_window_rect_visible(
                    w.hwnd,
                    Rect::new(
                        off_bezel.0,
                        off_bezel.1,
                        off_bezel.0 + w.target_rect.width(),
                        off_bezel.1 + w.target_rect.height(),
                    ),
                );
                WindowTransition {
                    hwnd: w.hwnd,
                    start_visible_x: off_bezel.0,
                    start_visible_y: off_bezel.1,
                    end_visible_x: w.target_rect.left,
                    end_visible_y: w.target_rect.top,
                }
            })
            .collect();

        // Prepare docked 28px header bar at off-bezel origin before DwmFlush() loop
        let hdr_off_bezel = compute_offbezel_origin(&monitor, key.1, target_header_rect);
        let off_header_rect = Rect::new(
            hdr_off_bezel.0,
            hdr_off_bezel.1,
            hdr_off_bezel.0 + target_header_rect.width(),
            hdr_off_bezel.1 + target_header_rect.height(),
        );
        self.emit_header_for_slot(key, slot_idx, Some(off_header_rect));
        if let Some(hdr_hwnd) = self.overlay.header_hwnd_for_edge(&key.0, key.1) {
            transitions.push(WindowTransition {
                hwnd: hdr_hwnd,
                start_visible_x: hdr_off_bezel.0,
                start_visible_y: hdr_off_bezel.1,
                end_visible_x: target_header_rect.left,
                end_visible_y: target_header_rect.top,
            });
        }

        if dock_mode == DockMode::FloatingOverlay && self.cfg.dim_background > 0 {
            self.overlay
                .emit_overlay_event(PillOverlayEvent::SetDimScrim {
                    visible: true,
                    monitor_rect: monitor.rect,
                    opacity_percent: self.cfg.dim_background,
                });
        }

        if matches!(
            dock_mode,
            DockMode::FloatingOverlay | DockMode::FloatingPinned
        ) {
            for w in &self.decks[key].slots[slot_idx].windows {
                self.windows.set_window_topmost(w.hwnd, true);
            }
        }

        self.windows.animate_transitions(
            &transitions,
            self.cfg.open_duration_ms,
            self.cfg.transition_style,
            true,
        );

        for w in &self.decks[key].slots[slot_idx].windows {
            self.windows.refresh_window_rendering(w.hwnd);
        }

        let primary_hwnd = self.decks[key].slots[slot_idx]
            .windows
            .first()
            .map(|w| w.hwnd);
        if mark_as_default && let Some(h) = primary_hwnd {
            self.last_default_opened_hwnd = Some(h);
        }
        self.decks.get_mut(key).unwrap().slots[slot_idx].is_open = true;
        self.emit_header_for_slot(key, slot_idx, None);

        if dock_mode == DockMode::FloatingOverlay {
            self.open_floating_edge = Some((key.0.clone(), key.1, slot_idx));
            self.dismissal.on_drawer_opened(previous_fg, dock_mode);
            if self.drag_active {
                self.dismissal.set_drag_in_progress(true);
            }
        }

        if allow_auto_focus
            && !self.drag_active
            && self.cfg.auto_focus_on_open
            && let Some(hwnd) = primary_hwnd
        {
            if let Some(slot_mut) = self
                .decks
                .get_mut(key)
                .and_then(|d| d.slots.get_mut(slot_idx))
            {
                for win in &mut slot_mut.windows {
                    win.needs_attention = false;
                    win.attention_time = None;
                }
            }
            self.dismissal.mark_drawer_gained_focus();
            self.windows.set_foreground_window(hwnd);

            // Re-assert target_rect post-activation so any OS window placement quirks
            // or SwitchToThisWindow restore artifacts are immediately clamped to the docked bezel!
            if let Some(w) = self.decks[key].slots[slot_idx]
                .windows
                .iter()
                .find(|w| w.hwnd == hwnd)
            {
                self.windows.set_window_rect_visible(hwnd, w.target_rect);
            }
        }

        self.sync_overlay_for_edge(key);
        Ok(())
    }

    fn close_slot(&mut self, key: &(String, ScreenEdge), slot_idx: usize) {
        let Some(slot) = self.decks.get(key).and_then(|d| d.slots.get(slot_idx)) else {
            return;
        };
        if !slot.is_open {
            return;
        }

        let monitors = self.windows.enumerate_monitors();
        let Some(monitor) = resolve_target_monitor(&monitors, Some(&key.0)).cloned() else {
            return;
        };

        let mut transitions: Vec<WindowTransition> = slot
            .windows
            .iter()
            .map(|w| {
                let off_bezel = compute_offbezel_origin(&monitor, key.1, w.target_rect);
                WindowTransition {
                    hwnd: w.hwnd,
                    start_visible_x: w.target_rect.left,
                    start_visible_y: w.target_rect.top,
                    end_visible_x: off_bezel.0,
                    end_visible_y: off_bezel.1,
                }
            })
            .collect();

        if let Some(hdr_hwnd) = self.overlay.header_hwnd_for_edge(&key.0, key.1) {
            let hdr_off = compute_offbezel_origin(&monitor, key.1, slot.header_rect);
            transitions.push(WindowTransition {
                hwnd: hdr_hwnd,
                start_visible_x: slot.header_rect.left,
                start_visible_y: slot.header_rect.top,
                end_visible_x: hdr_off.0,
                end_visible_y: hdr_off.1,
            });
        }

        self.windows.animate_transitions(
            &transitions,
            self.cfg.close_duration_ms,
            self.cfg.transition_style,
            false,
        );

        self.overlay
            .emit_overlay_event(PillOverlayEvent::HideDrawerHeader {
                monitor_id: key.0.clone(),
                edge: key.1,
            });

        let vbounds = virtual_desktop_bounds(&monitors);
        for w in &self.decks[key].slots[slot_idx].windows {
            let parked = compute_parked_rect(vbounds, w.target_rect);
            self.windows.park_and_cloak_window(w.hwnd, parked);
        }

        self.decks.get_mut(key).unwrap().slots[slot_idx].is_open = false;

        if self.open_floating_edge.as_ref() == Some(&(key.0.clone(), key.1, slot_idx)) {
            self.open_floating_edge = None;
            self.dismissal.reset();
            self.hide_dim_scrim();
        }

        self.sync_overlay_for_edge(key);
    }

    fn hide_dim_scrim(&self) {
        if self.cfg.dim_background > 0 {
            self.overlay
                .emit_overlay_event(PillOverlayEvent::SetDimScrim {
                    visible: false,
                    monitor_rect: Rect::default(),
                    opacity_percent: 0,
                });
        }
    }

    fn cleanup_after_window_removal(
        &mut self,
        key: &(String, ScreenEdge),
        slot_idx: usize,
        prev_thickness: i32,
    ) {
        let should_remove_slot = self
            .decks
            .get(key)
            .is_some_and(|d| d.slots[slot_idx].windows.is_empty());

        if should_remove_slot {
            let removed_slot = self.decks.get_mut(key).unwrap().slots.remove(slot_idx);
            if removed_slot.is_open {
                self.overlay
                    .emit_overlay_event(PillOverlayEvent::HideDrawerHeader {
                        monitor_id: key.0.clone(),
                        edge: key.1,
                    });
            }
            if removed_slot.dock_mode == DockMode::NativeWorkAreaDock {
                let monitors = self.windows.enumerate_monitors();
                let mon_rect = resolve_target_monitor(&monitors, Some(&key.0))
                    .map(|m| m.rect)
                    .unwrap_or_default();
                self.appbar
                    .register_or_update_dock(&key.0, key.1, mon_rect, prev_thickness, false);
            }
            if let Some((open_mon, open_edge, open_idx)) = self.open_floating_edge.clone()
                && (open_mon, open_edge) == (key.0.clone(), key.1)
            {
                if open_idx == slot_idx {
                    self.open_floating_edge = None;
                    self.dismissal.reset();
                    self.hide_dim_scrim();
                } else if open_idx > slot_idx {
                    self.open_floating_edge = Some((key.0.clone(), key.1, open_idx - 1));
                }
            }
            let deck = self.decks.get_mut(key).unwrap();
            if deck.slots.is_empty() {
                self.decks.remove(key);
            } else if slot_idx < deck.active_index {
                deck.active_index -= 1;
            } else {
                deck.active_index = deck.active_index.min(deck.slots.len() - 1);
            }
        } else {
            self.recompute_slot_geometry(key, slot_idx);
            if self.decks[key].slots[slot_idx].is_open {
                for w in &self.decks[key].slots[slot_idx].windows {
                    self.windows.set_window_rect_visible(w.hwnd, w.target_rect);
                }
            }
        }
        self.sync_overlay_for_edge(key);
    }

    fn recompute_slot_geometry(&mut self, key: &(String, ScreenEdge), slot_idx: usize) {
        let monitors = self.windows.enumerate_monitors();
        let Some(mon) = resolve_target_monitor(&monitors, Some(&key.0)) else {
            return;
        };
        let Some(slot) = self
            .decks
            .get_mut(key)
            .and_then(|d| d.slots.get_mut(slot_idx))
        else {
            return;
        };
        let thicknesses: Vec<i32> = slot.windows.iter().map(|w| w.thickness_px).collect();
        let reserved = if slot.dock_mode == DockMode::NativeWorkAreaDock {
            slot.docked_reserved_rect
        } else {
            None
        };
        let (header_rect, rects) =
            compute_slot_rects_on_monitor(mon, key.1, &thicknesses, reserved);
        slot.header_rect = header_rect;
        for (w, r) in slot.windows.iter_mut().zip(rects) {
            w.target_rect = r;
        }
    }

    fn emit_header_for_slot(
        &self,
        key: &(String, ScreenEdge),
        slot_idx: usize,
        override_rect: Option<Rect>,
    ) {
        let Some(deck) = self.decks.get(key) else {
            return;
        };
        let Some(slot) = deck.slots.get(slot_idx) else {
            return;
        };
        let visible_indices = self.visible_slot_indices(key);
        let visible_pos = visible_indices
            .iter()
            .position(|&i| i == slot_idx)
            .unwrap_or(0);
        let base_title = slot
            .windows
            .first()
            .map(|w| w.title.clone())
            .unwrap_or_else(|| "EdgeStash Drawer".to_string());
        let is_grouped = slot.windows.len() > 1;
        let title = if is_grouped {
            format!("{base_title} (+{})", slot.windows.len() - 1)
        } else {
            base_title
        };
        self.overlay
            .emit_overlay_event(PillOverlayEvent::ShowDrawerHeader {
                monitor_id: key.0.clone(),
                edge: key.1,
                header_rect: override_rect.unwrap_or(slot.header_rect),
                title,
                slot_index: visible_pos,
                slot_count: visible_indices.len().max(1),
                dock_mode: slot.dock_mode,
                preferred_pin_mode: slot.preferred_pin_mode,
                is_grouped,
            });
    }

    fn sync_overlay_for_edge(&self, key: &(String, ScreenEdge)) {
        let visible_indices = self.visible_slot_indices(key);
        let deck = self.decks.get(key);
        let active_idx = deck.map(|d| d.active_index).unwrap_or(0);
        let is_open = deck
            .and_then(|d| d.slots.get(active_idx))
            .is_some_and(|s| s.is_open);
        let mut window_handles = Vec::new();
        let mut slot_titles = Vec::new();
        let mut needs_attention = Vec::new();
        let mut attention_count = 0u32;
        if let Some(d) = deck {
            for &i in &visible_indices {
                if let Some(s) = d.slots.get(i)
                    && let Some(first) = s.windows.first()
                {
                    window_handles.push(first.hwnd);
                    slot_titles.push(first.title.clone());
                    let has_att = s.needs_attention();
                    needs_attention.push(has_att);
                    if has_att {
                        attention_count += 1;
                    }
                }
            }
        }

        self.overlay
            .emit_overlay_event(PillOverlayEvent::SyncState {
                monitor_id: key.0.clone(),
                edge: key.1,
                slot_count: visible_indices.len(),
                active_index: visible_indices
                    .iter()
                    .position(|&i| i == active_idx)
                    .unwrap_or(0),
                window_handles,
                slot_titles,
                is_open,
                needs_attention,
                attention_count,
            });
    }

    fn visible_slot_indices(&self, key: &(String, ScreenEdge)) -> Vec<usize> {
        let Some(deck) = self.decks.get(key) else {
            return Vec::new();
        };
        let current_desktop = self.vdesktop.get_current_desktop_id();
        deck.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| match slot.vd_scope {
                VirtualDesktopScope::AllDesktops => Some(i),
                VirtualDesktopScope::PinnedToDesktop(guid_bytes) => {
                    let slot_guid = edgestash_ipc::types::bytes_to_guid(guid_bytes);
                    let matches = current_desktop == Some(slot_guid)
                        || slot
                            .windows
                            .first()
                            .is_some_and(|w| self.vdesktop.is_on_current_desktop(w.hwnd));
                    matches.then_some(i)
                }
            })
            .collect()
    }

    fn active_visible_slot_index(&self, key: &(String, ScreenEdge)) -> Option<usize> {
        let visible = self.visible_slot_indices(key);
        if visible.is_empty() {
            return None;
        }
        if self.cfg.prioritize_attention_on_summon
            && let Some(deck) = self.decks.get(key)
            && let Some(&att_idx) = visible
                .iter()
                .find(|&&i| deck.slots.get(i).is_some_and(|s| s.needs_attention()))
        {
            return Some(att_idx);
        }
        let active = self.decks.get(key)?.active_index;
        if visible.contains(&active) {
            Some(active)
        } else {
            visible.first().copied()
        }
    }

    fn resolve_deck_key(
        &self,
        monitor_id: Option<&str>,
        edge: ScreenEdge,
    ) -> Result<(String, ScreenEdge), EngineError> {
        if let Some(id) = monitor_id {
            let key = (id.to_string(), edge);
            if self.decks.contains_key(&key) {
                return Ok(key);
            }
        } else {
            let monitors = self.windows.enumerate_monitors();
            if let Some(primary) = resolve_target_monitor(&monitors, None) {
                let pkey = (primary.id.clone(), edge);
                if self.decks.contains_key(&pkey) {
                    return Ok(pkey);
                }
            }
            if let Some(any) = self.decks.keys().find(|(_, e)| *e == edge) {
                return Ok(any.clone());
            }
        }
        Err(EngineError(
            IpcErrorCode::NoSlotOnEdge,
            format!("No stashed slot on {edge:?}"),
        ))
    }

    /// Returns `true` if `hwnd` is currently tracked in any stashed slot.
    pub fn is_window_stashed(&self, hwnd: WindowHandle) -> bool {
        self.find_window_slot(hwnd).is_some()
    }

    /// Returns `true` if `hwnd` is currently stashed and in an open drawer.
    pub fn is_window_stashed_and_open(&self, hwnd: WindowHandle) -> bool {
        if let Some((key, slot_idx, _)) = self.find_window_slot(hwnd) {
            self.decks
                .get(&key)
                .is_some_and(|d| d.slots.get(slot_idx).is_some_and(|s| s.is_open))
        } else {
            false
        }
    }

    fn find_window_slot(&self, hwnd: WindowHandle) -> Option<((String, ScreenEdge), usize, usize)> {
        for (key, deck) in &self.decks {
            for (s_idx, slot) in deck.slots.iter().enumerate() {
                if let Some(w_idx) = slot.windows.iter().position(|w| w.hwnd == hwnd) {
                    return Some((key.clone(), s_idx, w_idx));
                }
            }
        }
        None
    }

    fn persist_manifest(&self) {
        let mut manifest = CrashManifest::default();
        for (key, deck) in &self.decks {
            for slot in &deck.slots {
                for w in &slot.windows {
                    manifest.entries.push(CrashRecoveryEntry {
                        hwnd: w.hwnd.0,
                        original_ex_style: w.original_ex_style,
                        original_rect: w.original_rect,
                        monitor_id: key.0.clone(),
                        edge: key.1,
                    });
                }
            }
        }
        let _ = manifest.save_atomic(&self.manifest_path);
    }
}

fn map_access_err(err: WindowAccessError) -> EngineError {
    match err {
        WindowAccessError::InvalidWindow => EngineError(
            IpcErrorCode::InvalidWindow,
            "Target window is no longer valid".to_string(),
        ),
        WindowAccessError::ProtectedShellWindow => EngineError(
            IpcErrorCode::WindowProtected,
            "Cannot stash Windows shell or protected window".to_string(),
        ),
        WindowAccessError::AccessDenied => EngineError(
            IpcErrorCode::WindowProtected,
            "Access denied stashing elevated window (UIPI)".to_string(),
        ),
    }
}

fn select_monitor_for_rect(monitors: &[MonitorInfo], rect: Rect) -> Option<&MonitorInfo> {
    let cx = rect.left + rect.width() / 2;
    let cy = rect.top + rect.height() / 2;
    monitors
        .iter()
        .find(|m| m.rect.contains_inclusive(cx, cy))
        .or_else(|| resolve_target_monitor(monitors, None))
}
