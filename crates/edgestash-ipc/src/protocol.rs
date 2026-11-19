use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::types::{ChurnZoneId, DockMode, Rect, ScreenEdge, VirtualDesktopMode};

/// Default Windows Named Pipe endpoint for daemon IPC.
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\EdgeStash";

/// Maximum accepted IPC frame payload size (1 MiB) to protect against malformed framing.
pub const MAX_IPC_FRAME_BYTES: usize = 1024 * 1024;

/// Structured IPC requests sent from `edgestash-cli` or future `edgestash-ui` to `edgestash-daemon`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcRequest {
    Ping,
    Status,
    StashForeground {
        edge: ScreenEdge,
        group_with_active: bool,
    },
    StashWindow {
        hwnd: isize,
        edge: ScreenEdge,
        group_with_active: bool,
    },
    UnstashWindow {
        hwnd: isize,
    },
    UnstashActive {
        monitor_id: Option<String>,
        edge: ScreenEdge,
    },
    UnstashAll,
    ToggleEdge {
        monitor_id: Option<String>,
        edge: ScreenEdge,
    },
    ToggleActive,
    CycleEdge {
        monitor_id: Option<String>,
        edge: ScreenEdge,
        delta: i32,
    },
    SetDockMode {
        monitor_id: Option<String>,
        edge: ScreenEdge,
        mode: DockMode,
    },
    SetVirtualDesktopMode {
        hwnd: isize,
        mode: VirtualDesktopMode,
    },
    UngroupWindow {
        hwnd: isize,
    },
    PreviewChurnVisual {
        enabled: bool,
        zone: Option<ChurnZoneId>,
        corner_zone_px: i32,
        slide_distance_px: i32,
        diagonal_angle_deg: i32,
    },
    SimulateWindowAttention {
        hwnd: Option<isize>,
        edge: Option<ScreenEdge>,
    },
    ReloadConfig,
    Shutdown,
}

/// Machine-readable error codes returned over IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcErrorCode {
    InvalidWindow,
    WindowProtected,
    AlreadyStashed,
    NotStashed,
    NoSlotOnEdge,
    ConfigError,
    InternalError,
}

/// Snapshot of a single stashed window inside a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashedWindowSnapshot {
    pub hwnd: isize,
    pub title: String,
    #[serde(default)]
    pub process_name: String,
    pub target_rect: Rect,
    #[serde(default)]
    pub needs_attention: bool,
}

/// Snapshot of a slot (1 or more grouped windows) on a `(monitor, edge)` deck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeSlotSnapshot {
    pub monitor_id: String,
    pub edge: ScreenEdge,
    pub slot_index: usize,
    pub is_active_in_deck: bool,
    pub is_open: bool,
    pub dock_mode: DockMode,
    pub virtual_desktop_mode: VirtualDesktopMode,
    #[serde(default)]
    pub needs_attention: bool,
    pub windows: Vec<StashedWindowSnapshot>,
}

/// Full daemon status snapshot returned for `IpcRequest::Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusSnapshot {
    pub pid: u32,
    pub slots: Vec<EdgeSlotSnapshot>,
}

/// Structured IPC responses from `edgestash-daemon`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcResponse {
    Pong { version: String, pid: u32 },
    Status(DaemonStatusSnapshot),
    Ok { message: String },
    Error { code: IpcErrorCode, message: String },
}

/// Encodes a serializable message into a 4-byte little-endian length-prefixed JSON frame.
pub fn encode_frame<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let payload = serde_json::to_vec(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IPC frame exceeds maximum size",
        ));
    }

    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC payload too large"))?;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decodes a complete length-prefixed JSON frame from an in-memory byte slice.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> io::Result<T> {
    if frame.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing IPC frame header",
        ));
    }
    let len = u32::from_le_bytes(frame[0..4].try_into().expect("4 bytes")) as usize;
    if len > MAX_IPC_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds maximum size",
        ));
    }
    if frame.len() < 4 + len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated IPC frame payload",
        ));
    }
    serde_json::from_slice(&frame[4..4 + len])
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Writes a length-prefixed JSON frame to any `Write` stream.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    let encoded = encode_frame(value)?;
    writer.write_all(&encoded)?;
    writer.flush()
}

/// Reads and deserializes a length-prefixed JSON frame from any `Read` stream.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_IPC_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds maximum size",
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}
