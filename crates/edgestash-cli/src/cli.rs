use std::process::ExitCode;

use edgestash_ipc::pipe::PipeClient;
use edgestash_ipc::protocol::{DEFAULT_PIPE_NAME, IpcRequest, IpcResponse};
use edgestash_ipc::types::{DockMode, ScreenEdge, VirtualDesktopMode};

/// Parsed CLI command ready for IPC dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCommand {
    pub request: IpcRequest,
    pub json_output: bool,
    pub pipe_name: String,
}

/// Parses command-line arguments (`argv[0]` included) into a [`CliCommand`].
pub fn parse_args<I>(args: I) -> Result<CliCommand, String>
where
    I: IntoIterator<Item = String>,
{
    let raw: Vec<String> = args.into_iter().skip(1).collect();
    if raw.is_empty() {
        return Err(usage_text());
    }

    let mut json_output = false;
    let mut pipe_name = DEFAULT_PIPE_NAME.to_string();
    let mut positionals: Vec<String> = Vec::new();
    let mut group_with_active = false;
    let mut all_flag = false;
    let mut prev_flag = false;
    let mut hwnd_flag: Option<isize> = None;
    let mut monitor_flag: Option<String> = None;

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--json" => json_output = true,
            "--group" => group_with_active = true,
            "--all" => all_flag = true,
            "--prev" => prev_flag = true,
            "--hwnd" => {
                i += 1;
                let val = raw.get(i).ok_or("--hwnd requires a value")?;
                hwnd_flag = Some(
                    val.parse::<isize>()
                        .map_err(|_| format!("invalid hwnd '{val}'"))?,
                );
            }
            "--monitor" => {
                i += 1;
                let val = raw.get(i).ok_or("--monitor requires a value")?;
                monitor_flag = Some(val.clone());
            }
            "--pipe" => {
                i += 1;
                let val = raw.get(i).ok_or("--pipe requires a pipe name")?;
                pipe_name = val.clone();
            }
            "--help" | "-h" => return Err(usage_text()),
            arg if arg.starts_with('-') => return Err(format!("unknown flag '{arg}'")),
            arg => positionals.push(arg.to_string()),
        }
        i += 1;
    }

    let cmd = positionals
        .first()
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(usage_text)?;

    let request = match cmd.as_str() {
        "ping" => IpcRequest::Ping,
        "status" => IpcRequest::Status,
        "stash" => {
            let edge_str = positionals.get(1).ok_or("stash requires an edge (left|right|top|bottom)")?;
            let edge = parse_edge(edge_str)?;
            match hwnd_flag {
                Some(hwnd) => IpcRequest::StashWindow {
                    hwnd,
                    edge,
                    group_with_active,
                },
                None => IpcRequest::StashForeground {
                    edge,
                    group_with_active,
                },
            }
        }
        "unstash" => {
            if all_flag {
                IpcRequest::UnstashAll
            } else if let Some(hwnd) = hwnd_flag {
                IpcRequest::UnstashWindow { hwnd }
            } else if let Some(edge_str) = positionals.get(1) {
                IpcRequest::UnstashActive {
                    monitor_id: monitor_flag,
                    edge: parse_edge(edge_str)?,
                }
            } else {
                return Err("unstash requires <edge>, --hwnd <id>, or --all".to_string());
            }
        }
        "toggle" => match positionals.get(1) {
            Some(edge_str) => IpcRequest::ToggleEdge {
                monitor_id: monitor_flag,
                edge: parse_edge(edge_str)?,
            },
            None => IpcRequest::ToggleActive,
        },
        "cycle" => {
            let edge_str = positionals.get(1).ok_or("cycle requires an edge")?;
            IpcRequest::CycleEdge {
                monitor_id: monitor_flag,
                edge: parse_edge(edge_str)?,
                delta: if prev_flag { -1 } else { 1 },
            }
        }
        "dock" => {
            let edge_str = positionals.get(1).ok_or("dock requires <edge> <floating|docked>")?;
            let mode_str = positionals.get(2).ok_or("dock requires <floating|docked>")?;
            IpcRequest::SetDockMode {
                monitor_id: monitor_flag,
                edge: parse_edge(edge_str)?,
                mode: parse_dock_mode(mode_str)?,
            }
        }
        "vdesktop" => {
            let hwnd_str = positionals.get(1).ok_or("vdesktop requires <hwnd> <all|current>")?;
            let mode_str = positionals.get(2).ok_or("vdesktop requires <all|current>")?;
            let hwnd = hwnd_str
                .parse::<isize>()
                .map_err(|_| format!("invalid hwnd '{hwnd_str}'"))?;
            let mode = match mode_str.to_ascii_lowercase().as_str() {
                "all" => VirtualDesktopMode::AllDesktops,
                "current" | "pin" | "pinned" => VirtualDesktopMode::CurrentDesktopOnly,
                _ => return Err(format!("invalid vdesktop mode '{mode_str}'")),
            };
            IpcRequest::SetVirtualDesktopMode { hwnd, mode }
        }
        "ungroup" => {
            let hwnd = if let Some(h) = hwnd_flag {
                h
            } else {
                let s = positionals.get(1).ok_or("ungroup requires <hwnd>")?;
                s.parse::<isize>()
                    .map_err(|_| format!("invalid hwnd '{s}'"))?
            };
            IpcRequest::UngroupWindow { hwnd }
        }
        "test-attention" | "alert" | "attention" => {
            let edge = positionals.get(1).and_then(|s| parse_edge(s).ok());
            IpcRequest::SimulateWindowAttention {
                hwnd: hwnd_flag,
                edge,
            }
        }
        "reload" => IpcRequest::ReloadConfig,
        "shutdown" => IpcRequest::Shutdown,
        other => return Err(format!("unknown command '{other}'\n\n{}", usage_text())),
    };

    Ok(CliCommand {
        request,
        json_output,
        pipe_name,
    })
}

fn parse_edge(raw: &str) -> Result<ScreenEdge, String> {
    match raw.to_ascii_lowercase().as_str() {
        "l" | "left" => Ok(ScreenEdge::Left),
        "r" | "right" => Ok(ScreenEdge::Right),
        "t" | "top" => Ok(ScreenEdge::Top),
        "b" | "bottom" => Ok(ScreenEdge::Bottom),
        _ => Err(format!("invalid edge '{raw}' (expected left|right|top|bottom)")),
    }
}

fn parse_dock_mode(raw: &str) -> Result<DockMode, String> {
    match raw.to_ascii_lowercase().as_str() {
        "floating" | "overlay" | "unpin" => Ok(DockMode::FloatingOverlay),
        "pinned" | "pin" | "floating-pin" => Ok(DockMode::FloatingPinned),
        "docked" | "dock" | "native" => Ok(DockMode::NativeWorkAreaDock),
        _ => Err(format!("invalid dock mode '{raw}' (expected floating|pinned|docked)")),
    }
}

fn usage_text() -> String {
    "Usage: edgestash-cli <command> [options]\n\
     Commands:\n\
       ping\n\
       status [--json]\n\
       stash <left|right|top|bottom> [--group] [--hwnd <id>]\n\
       unstash [<left|right|top|bottom> | --hwnd <id> | --all]\n\
       toggle [<left|right|top|bottom>] [--monitor <id>]\n\
       cycle <left|right|top|bottom> [--prev] [--monitor <id>]\n\
       dock <left|right|top|bottom> <floating|docked> [--monitor <id>]\n\
       vdesktop <hwnd> <all|current>\n\
       ungroup <hwnd>\n\
       test-attention [<left|right|top|bottom>] [--hwnd <id>]\n\
       reload\n\
       shutdown"
        .to_string()
}

/// Formats an [`IpcResponse`] for terminal display.
pub fn format_response(resp: &IpcResponse, json_output: bool) -> String {
    if json_output {
        return serde_json::to_string_pretty(resp).unwrap_or_else(|_| "{}" .to_string());
    }
    match resp {
        IpcResponse::Pong { version, pid } => format!("pong v{version} (pid={pid})"),
        IpcResponse::Ok { message } => message.clone(),
        IpcResponse::Error { code, message } => format!("error [{code:?}]: {message}"),
        IpcResponse::Status(st) => {
            if st.slots.is_empty() {
                return format!("daemon pid={} | no stashed windows", st.pid);
            }
            let mut lines = vec![format!("daemon pid={} | {} slot(s):", st.pid, st.slots.len())];
            for slot in &st.slots {
                let titles: Vec<String> = slot
                    .windows
                    .iter()
                    .map(|w| format!("{} ({})", w.title, w.hwnd))
                    .collect();
                lines.push(format!(
                    "  [{} {:?} #{}] open={} active={} mode={:?}{} | {}",
                    slot.monitor_id,
                    slot.edge,
                    slot.slot_index,
                    slot.is_open,
                    slot.is_active_in_deck,
                    slot.dock_mode,
                    if slot.needs_attention { " [ATTENTION]" } else { "" },
                    titles.join(" + ")
                ));
            }
            lines.join("\n")
        }
    }
}

/// Executes the CLI against the running daemon.
pub fn run_cli() -> ExitCode {
    let cmd = match parse_args(std::env::args()) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    let client = PipeClient::new(&cmd.pipe_name);
    match client.send(&cmd.request) {
        Ok(resp) => {
            let is_err = matches!(resp, IpcResponse::Error { .. });
            println!("{}", format_response(&resp, cmd.json_output));
            if is_err {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            eprintln!("failed to reach daemon on {}: {err}", cmd.pipe_name);
            ExitCode::from(1)
        }
    }
}
