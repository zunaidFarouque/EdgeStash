use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::ptr;

use windows::Win32::Foundation::{
    BOOL, CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, FlushFileBuffers, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows::core::PCWSTR;

use crate::protocol::{IpcRequest, IpcResponse, read_frame, write_frame};

const PIPE_BUFFER_SIZE: u32 = 65_536;
// Grants Full Access to Administrators/SYSTEM and Read+Write to Authenticated Users at Medium integrity.
const CROSS_INTEGRITY_SDDL: &str = "D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GRGW;;;AU)S:(ML;;NW;;;ME)";
const FALLBACK_DACL_SDDL: &str = "D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GRGW;;;AU)";

struct OwnedHandle(HANDLE);

// Win32 file/pipe kernel handles are process-global and safe to move between threads with exclusive ownership.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

impl Read for OwnedHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0u32;
        let to_read = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        unsafe {
            ReadFile(
                self.0,
                Some(&mut buf[..to_read as usize]),
                Some(&mut bytes_read),
                None,
            )
            .map_err(win32_err_to_io)?;
        }
        Ok(bytes_read as usize)
    }
}

impl Write for OwnedHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written = 0u32;
        let to_write = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        unsafe {
            WriteFile(
                self.0,
                Some(&buf[..to_write as usize]),
                Some(&mut bytes_written),
                None,
            )
            .map_err(win32_err_to_io)?;
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        unsafe { FlushFileBuffers(self.0).map_err(win32_err_to_io) }
    }
}

struct OwnedSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.0.0));
            }
        }
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32_err_to_io(err: windows::core::Error) -> io::Error {
    let raw = err.code().0 as u32;
    // HRESULT_FROM_WIN32 uses 0x8007xxxx
    let win32_code = if (raw & 0xFFFF_0000) == 0x8007_0000 {
        raw & 0x0000_FFFF
    } else {
        raw
    };
    if win32_code == ERROR_FILE_NOT_FOUND.0 {
        io::Error::new(io::ErrorKind::NotFound, err.message())
    } else {
        io::Error::from_raw_os_error(win32_code as i32)
    }
}

fn build_pipe_security_descriptor() -> Option<OwnedSecurityDescriptor> {
    for sddl in [CROSS_INTEGRITY_SDDL, FALLBACK_DACL_SDDL] {
        let wide = to_wide_null(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        };
        if ok.is_ok() && !sd.0.is_null() {
            return Some(OwnedSecurityDescriptor(sd));
        }
    }
    None
}

/// Synchronous Win32 Named Pipe server accepting one framed JSON request per connection.
pub struct PipeServer {
    handle: OwnedHandle,
    pipe_name: String,
}

impl PipeServer {
    /// Creates and binds the Named Pipe server with cross-integrity permissions.
    pub fn bind(pipe_name: &str) -> io::Result<Self> {
        let wide_name = to_wide_null(pipe_name);
        let sd_guard = build_pipe_security_descriptor();
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd_guard
                .as_ref()
                .map(|sd| sd.0.0)
                .unwrap_or(ptr::null_mut::<c_void>()),
            bInheritHandle: BOOL(0),
        };
        let sa_ptr = sd_guard
            .as_ref()
            .map(|_| &mut sa as *mut SECURITY_ATTRIBUTES);

        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                0,
                sa_ptr.map(|p| p as *const _),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            handle: OwnedHandle(handle),
            pipe_name: pipe_name.to_string(),
        })
    }

    /// Returns the bound pipe path.
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Waits for a single client connection, reads one `IpcRequest`, invokes `handler`,
    /// writes the `IpcResponse`, and disconnects the client handle for reuse.
    pub fn accept_one<F>(&mut self, mut handler: F) -> io::Result<IpcRequest>
    where
        F: FnMut(IpcRequest) -> IpcResponse,
    {
        let connect_res = unsafe { ConnectNamedPipe(self.handle.0, None) };
        if let Err(err) = connect_res {
            let last = unsafe { GetLastError() };
            if last != ERROR_PIPE_CONNECTED {
                return Err(win32_err_to_io(err));
            }
        }

        let result = (|| {
            let req: IpcRequest = read_frame(&mut self.handle)?;
            let resp = handler(req.clone());
            write_frame(&mut self.handle, &resp)?;
            Ok(req)
        })();

        unsafe {
            let _ = FlushFileBuffers(self.handle.0);
            let _ = DisconnectNamedPipe(self.handle.0);
        }

        result
    }
}

/// Synchronous Win32 Named Pipe client.
pub struct PipeClient {
    pipe_name: String,
}

impl PipeClient {
    /// Creates a client targeting `pipe_name` (e.g. `DEFAULT_PIPE_NAME`).
    pub fn new(pipe_name: impl Into<String>) -> Self {
        Self {
            pipe_name: pipe_name.into(),
        }
    }

    /// Connects to the daemon pipe, sends `request`, and reads back the `IpcResponse`.
    pub fn send(&self, request: &IpcRequest) -> io::Result<IpcResponse> {
        let wide_name = to_wide_null(&self.pipe_name);
        let mut handle = self.open_pipe_handle(&wide_name)?;
        write_frame(&mut handle, request)?;
        read_frame(&mut handle)
    }

    fn open_pipe_handle(&self, wide_name: &[u16]) -> io::Result<OwnedHandle> {
        for attempt in 0..3 {
            let res = unsafe {
                CreateFileW(
                    PCWSTR(wide_name.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            };

            match res {
                Ok(h) if h != INVALID_HANDLE_VALUE => return Ok(OwnedHandle(h)),
                Ok(_) => return Err(io::Error::last_os_error()),
                Err(err) => {
                    let os_err = win32_err_to_io(err);
                    if os_err.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) && attempt < 2 {
                        unsafe {
                            let _ = WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), 1000);
                        }
                        continue;
                    }
                    return Err(os_err);
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "named pipe remained busy",
        ))
    }
}
