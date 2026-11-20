use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use edgestash_ipc::config::EdgeStashConfig;
use edgestash_ipc::protocol::{DaemonStatusSnapshot, IpcRequest, IpcResponse};

use crate::hotkey::{HotkeyField, validate_hotkey_config};
use crate::ipc_bridge::DaemonClient;

/// Navigation tabs in the `edgestash-ui` Settings & Control Center window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTab {
    Dashboard,
    Triggers,
    Behavior,
    Visuals,
    Hotkeys,
}

impl UiTab {
    pub const ALL: [Self; 5] = [
        Self::Dashboard,
        Self::Triggers,
        Self::Behavior,
        Self::Visuals,
        Self::Hotkeys,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Live Slots",
            Self::Triggers => "Triggers",
            Self::Behavior => "Behavior & Focus",
            Self::Visuals => "Visuals & Pill",
            Self::Hotkeys => "Hotkeys",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Dashboard => "Inspect & control stashed windows on screen edges",
            Self::Triggers => "Corner slide, edge hover & fullscreen mute",
            Self::Behavior => "Animations, auto-hide & focus restoration",
            Self::Visuals => "Edge pill indicator & backdrop dimming",
            Self::Hotkeys => "Global keyboard shortcuts for instant stashing",
        }
    }
}

/// Live connection state of the background `edgestash-daemon`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonConnection {
    Unknown,
    Connected { version: String, pid: u32 },
    Disconnected { reason: String },
}

/// Severity level for status feedback banners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Success,
    Warning,
    Error,
}

/// Inline status banner shown in the top/bottom area of the configurator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusBanner {
    pub kind: BannerKind,
    pub message: String,
}

/// Pure testable ViewModel owning configuration drafts, validation, and daemon snapshot state.
#[derive(Debug, Clone)]
pub struct EdgeStashUiModel {
    pub config_path: PathBuf,
    pub saved_config: EdgeStashConfig,
    pub draft_config: EdgeStashConfig,
    pub hotkey_errors: BTreeMap<HotkeyField, String>,
    pub active_tab: UiTab,
    pub connection: DaemonConnection,
    pub status_snapshot: Option<DaemonStatusSnapshot>,
    pub banner: Option<StatusBanner>,
    pub capturing_hotkey: Option<HotkeyField>,
}

impl EdgeStashUiModel {
    pub fn new(config_path: PathBuf, saved_config: EdgeStashConfig) -> Self {
        Self {
            config_path,
            draft_config: saved_config.clone(),
            saved_config,
            hotkey_errors: BTreeMap::new(),
            active_tab: UiTab::Dashboard,
            connection: DaemonConnection::Unknown,
            status_snapshot: None,
            banner: None,
            capturing_hotkey: None,
        }
    }

    /// Loads `config.toml` from `config_path` (creating defaults if missing).
    pub fn load_or_create(config_path: PathBuf) -> Result<Self, String> {
        let cfg = EdgeStashConfig::load_or_create_at(&config_path)
            .map_err(|err| format!("Failed loading {}: {err}", config_path.display()))?;
        Ok(Self::new(config_path, cfg))
    }

    /// Returns true if `draft_config` differs from the last persisted configuration.
    pub fn is_dirty(&self) -> bool {
        self.draft_config != self.saved_config
    }

    /// Validates all draft hotkey expressions and refreshes `hotkey_errors`.
    pub fn validate_draft(&mut self) -> bool {
        self.hotkey_errors = validate_hotkey_config(&self.draft_config.hotkeys);
        self.hotkey_errors.is_empty()
    }

    /// Reverts uncommitted draft edits back to `saved_config`.
    pub fn discard_changes(&mut self) {
        self.draft_config = self.saved_config.clone();
        self.hotkey_errors.clear();
        self.capturing_hotkey = None;
        self.banner = Some(StatusBanner {
            kind: BannerKind::Info,
            message: "Discarded unsaved edits.".to_string(),
        });
    }

    /// Resets the draft configuration to `EdgeStashConfig::default()` without saving immediately.
    pub fn reset_to_defaults(&mut self) {
        self.draft_config = EdgeStashConfig::default();
        self.hotkey_errors.clear();
        self.capturing_hotkey = None;
        self.banner = Some(StatusBanner {
            kind: BannerKind::Info,
            message: "Loaded default settings into draft — click Save & Apply to persist."
                .to_string(),
        });
    }

    /// Validates & saves `draft_config` locally, then notifies daemon to reload if available.
    pub fn apply_and_save(&mut self, client: &dyn DaemonClient) -> Result<(), String> {
        self.save_draft_to_disk()?;
        let reload_res = client.send_request(&IpcRequest::ReloadConfig);
        self.apply_reload_outcome(reload_res);
        Ok(())
    }

    /// Validates hotkeys, clamps ranges, and writes `config.toml` locally without blocking on IPC.
    pub fn save_draft_to_disk(&mut self) -> Result<(), String> {
        if !self.validate_draft() {
            let msg = format!(
                "Fix {} invalid hotkey binding(s) before saving.",
                self.hotkey_errors.len()
            );
            self.banner = Some(StatusBanner {
                kind: BannerKind::Error,
                message: msg.clone(),
            });
            return Err(msg);
        }

        self.draft_config.validate_and_clamp();
        self.draft_config
            .save_to_at(&self.config_path)
            .map_err(|err| {
                let msg = format!("Failed saving {}: {err}", self.config_path.display());
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Error,
                    message: msg.clone(),
                });
                msg
            })?;
        self.saved_config = self.draft_config.clone();
        self.capturing_hotkey = None;
        Ok(())
    }

    /// Saves the current draft configuration as a named preset file in the presets directory.
    pub fn save_as_named_preset(&mut self, preset_name: &str) -> Result<PathBuf, String> {
        if !self.validate_draft() {
            let msg = format!(
                "Fix {} invalid hotkey binding(s) before saving preset.",
                self.hotkey_errors.len()
            );
            self.banner = Some(StatusBanner {
                kind: BannerKind::Error,
                message: msg.clone(),
            });
            return Err(msg);
        }

        self.draft_config.validate_and_clamp();
        let path = self
            .draft_config
            .save_as_named_preset(preset_name)
            .map_err(|err| format!("Failed to save preset: {err}"))?;
        Ok(path)
    }

    /// Loads a preset from `path` into `draft_config`.
    pub fn load_preset_from_disk(&mut self, path: &Path) -> Result<(), String> {
        let loaded = EdgeStashConfig::load_or_create_at(path)
            .map_err(|err| format!("Failed to load preset {}: {err}", path.display()))?;
        self.draft_config = loaded;
        self.validate_draft();
        Ok(())
    }

    /// Updates banner and connection state from a `ReloadConfig` IPC response.
    pub fn apply_reload_outcome(&mut self, res: std::io::Result<IpcResponse>) {
        match res {
            Ok(IpcResponse::Ok { .. }) => {
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Success,
                    message: "Settings saved and applied live to running daemon.".to_string(),
                });
            }
            Ok(IpcResponse::Error { message, .. }) => {
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Warning,
                    message: format!(
                        "Saved config.toml, but daemon reload returned error: {message}"
                    ),
                });
            }
            Ok(_) => {
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Success,
                    message: "Settings saved to config.toml.".to_string(),
                });
            }
            Err(err) => {
                self.connection = DaemonConnection::Disconnected {
                    reason: err.to_string(),
                };
                self.status_snapshot = None;
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Warning,
                    message: "Saved config.toml (daemon offline — changes take effect when daemon starts)."
                        .to_string(),
                });
            }
        }
    }

    /// Queries daemon `Ping` and `Status` synchronously.
    pub fn refresh_daemon(&mut self, client: &dyn DaemonClient) {
        let ping = client.send_request(&IpcRequest::Ping);
        let status = match &ping {
            Ok(IpcResponse::Pong { .. }) => Some(client.send_request(&IpcRequest::Status)),
            _ => None,
        };
        self.apply_poll_outcome(ping, status);
    }

    /// Applies pre-fetched `Ping` + optional `Status` IPC responses to the model.
    pub fn apply_poll_outcome(
        &mut self,
        ping: std::io::Result<IpcResponse>,
        status: Option<std::io::Result<IpcResponse>>,
    ) {
        match ping {
            Ok(IpcResponse::Pong { version, pid }) => {
                self.connection = DaemonConnection::Connected { version, pid };
                match status {
                    Some(Ok(IpcResponse::Status(snap))) => {
                        self.status_snapshot = Some(snap);
                    }
                    Some(Ok(IpcResponse::Error { message, .. })) => {
                        self.banner = Some(StatusBanner {
                            kind: BannerKind::Warning,
                            message,
                        });
                    }
                    Some(Err(err)) => {
                        self.connection = DaemonConnection::Disconnected {
                            reason: err.to_string(),
                        };
                        self.status_snapshot = None;
                    }
                    _ => {}
                }
            }
            Ok(IpcResponse::Error { message, .. }) => {
                self.connection = DaemonConnection::Disconnected { reason: message };
                self.status_snapshot = None;
            }
            Ok(_) => {}
            Err(err) => {
                self.connection = DaemonConnection::Disconnected {
                    reason: err.to_string(),
                };
                self.status_snapshot = None;
            }
        }
    }

    /// Executes an IPC action synchronously and refreshes the status snapshot if it succeeded.
    pub fn dispatch_action(&mut self, client: &dyn DaemonClient, req: IpcRequest) {
        let is_shutdown = matches!(req, IpcRequest::Shutdown);
        let action_res = client.send_request(&req);
        let refresh_res = match (&action_res, is_shutdown) {
            (Ok(IpcResponse::Ok { .. }), false) => Some(client.send_request(&IpcRequest::Status)),
            _ => None,
        };
        self.apply_action_outcome(is_shutdown, action_res, refresh_res);
    }

    /// Applies pre-fetched action + optional refreshed status results to the model.
    pub fn apply_action_outcome(
        &mut self,
        was_shutdown: bool,
        action_res: std::io::Result<IpcResponse>,
        refresh_res: Option<std::io::Result<IpcResponse>>,
    ) {
        match action_res {
            Ok(IpcResponse::Ok { message }) => {
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Success,
                    message,
                });
                if was_shutdown {
                    self.connection = DaemonConnection::Disconnected {
                        reason: "Daemon stopped".to_string(),
                    };
                    self.status_snapshot = None;
                } else if let Some(Ok(IpcResponse::Status(snap))) = refresh_res {
                    self.status_snapshot = Some(snap);
                }
            }
            Ok(IpcResponse::Error { message, .. }) => {
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Error,
                    message,
                });
            }
            Ok(other) => {
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Info,
                    message: format!("Daemon response: {other:?}"),
                });
            }
            Err(err) => {
                self.connection = DaemonConnection::Disconnected {
                    reason: err.to_string(),
                };
                self.status_snapshot = None;
                self.banner = Some(StatusBanner {
                    kind: BannerKind::Error,
                    message: format!("Daemon unreachable: {err}"),
                });
            }
        }
    }
}
