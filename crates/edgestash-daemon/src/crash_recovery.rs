use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use edgestash_ipc::types::{Rect, ScreenEdge};
use serde::{Deserialize, Serialize};

use crate::platform::WindowHandle;

/// Single stashed window entry persisted immediately on stash for crash/kill recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashRecoveryEntry {
    pub hwnd: isize,
    pub original_ex_style: isize,
    pub original_rect: Rect,
    pub monitor_id: String,
    pub edge: ScreenEdge,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub process_name: String,
}

/// On-disk recovery manifest (`%LOCALAPPDATA%\EdgeStash\active_stashes.json`).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashManifest {
    pub entries: Vec<CrashRecoveryEntry>,
}

impl CrashManifest {
    /// Returns `%LOCALAPPDATA%\EdgeStash\active_stashes.json`.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|dir| dir.join("EdgeStash").join("active_stashes.json"))
    }

    /// Loads manifest from `path`, returning an empty manifest if the file does not exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    /// Atomically persists the recovery manifest to `path` via a unique temp sibling file and atomic rename.
    pub fn save_atomic(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_vec_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        let tmp_path = path.with_extension(format!("{}.tmp", std::process::id()));
        {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&payload)?;
            file.sync_all()?;
        }
        fs::rename(&tmp_path, path)
    }
}

/// Recovers any leftover stashed windows from an unclean prior daemon termination
/// and clears the manifest.
pub fn recover_orphans_with<IsValid, Restore>(
    manifest_path: &Path,
    mut is_window_valid: IsValid,
    mut restore_window: Restore,
) -> io::Result<usize>
where
    IsValid: FnMut(WindowHandle) -> bool,
    Restore: FnMut(WindowHandle, isize, Rect),
{
    let manifest = CrashManifest::load(manifest_path)?;
    if manifest.entries.is_empty() {
        return Ok(0);
    }

    let mut restored_count = 0usize;
    for entry in &manifest.entries {
        let hwnd = WindowHandle(entry.hwnd);
        if is_window_valid(hwnd) {
            restore_window(hwnd, entry.original_ex_style, entry.original_rect);
            restored_count += 1;
        }
    }

    CrashManifest::default().save_atomic(manifest_path)?;
    Ok(restored_count)
}
