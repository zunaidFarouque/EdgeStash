use edgestash_cli::cli::{CliCommand, format_response, parse_args};
use edgestash_ipc::protocol::{
    DaemonStatusSnapshot, EdgeSlotSnapshot, IpcErrorCode, IpcRequest, IpcResponse,
    StashedWindowSnapshot,
};
use edgestash_ipc::types::{DockMode, Rect, ScreenEdge, VirtualDesktopMode};

fn parse(items: &[&str]) -> Result<CliCommand, String> {
    parse_args(items.iter().map(|s| (*s).to_string()))
}

#[test]
fn parses_all_core_cli_subcommands_into_ipc_requests() {
    assert_eq!(
        parse(&["edgestash-cli", "ping"]).unwrap().request,
        IpcRequest::Ping
    );

    let status = parse(&["edgestash-cli", "status", "--json"]).unwrap();
    assert_eq!(status.request, IpcRequest::Status);
    assert!(status.json_output);

    assert_eq!(
        parse(&["edgestash-cli", "stash", "right", "--group"])
            .unwrap()
            .request,
        IpcRequest::StashForeground {
            edge: ScreenEdge::Right,
            group_with_active: true,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "stash", "l", "--hwnd", "1234"])
            .unwrap()
            .request,
        IpcRequest::StashWindow {
            hwnd: 1234,
            edge: ScreenEdge::Left,
            group_with_active: false,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "unstash", "--all"])
            .unwrap()
            .request,
        IpcRequest::UnstashAll
    );

    assert_eq!(
        parse(&["edgestash-cli", "unstash", "--hwnd", "55"])
            .unwrap()
            .request,
        IpcRequest::UnstashWindow { hwnd: 55 }
    );

    assert_eq!(
        parse(&["edgestash-cli", "toggle"]).unwrap().request,
        IpcRequest::ToggleActive
    );

    assert_eq!(
        parse(&["edgestash-cli", "toggle", "bottom", "--monitor", "MON-2"])
            .unwrap()
            .request,
        IpcRequest::ToggleEdge {
            monitor_id: Some("MON-2".to_string()),
            edge: ScreenEdge::Bottom,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "cycle", "top", "--prev"])
            .unwrap()
            .request,
        IpcRequest::CycleEdge {
            monitor_id: None,
            edge: ScreenEdge::Top,
            delta: -1,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "dock", "left", "docked"])
            .unwrap()
            .request,
        IpcRequest::SetDockMode {
            monitor_id: None,
            edge: ScreenEdge::Left,
            mode: DockMode::NativeWorkAreaDock,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "vdesktop", "99", "current"])
            .unwrap()
            .request,
        IpcRequest::SetVirtualDesktopMode {
            hwnd: 99,
            mode: VirtualDesktopMode::CurrentDesktopOnly,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "ungroup", "99"]).unwrap().request,
        IpcRequest::UngroupWindow { hwnd: 99 }
    );

    assert_eq!(
        parse(&["edgestash-cli", "reload"]).unwrap().request,
        IpcRequest::ReloadConfig
    );

    assert_eq!(
        parse(&["edgestash-cli", "shutdown"]).unwrap().request,
        IpcRequest::Shutdown
    );

    assert_eq!(
        parse(&["edgestash-cli", "test-attention"]).unwrap().request,
        IpcRequest::SimulateWindowAttention {
            hwnd: None,
            edge: None,
        }
    );

    assert_eq!(
        parse(&["edgestash-cli", "alert", "right", "--hwnd", "888"])
            .unwrap()
            .request,
        IpcRequest::SimulateWindowAttention {
            hwnd: Some(888),
            edge: Some(ScreenEdge::Right),
        }
    );

    assert!(parse(&["edgestash-cli"]).is_err());
    assert!(parse(&["edgestash-cli", "stash", "diagonal"]).is_err());
}

#[test]
fn formats_ipc_responses_for_human_and_json_output() {
    let pong = format_response(
        &IpcResponse::Pong {
            version: "0.1.0".to_string(),
            pid: 4242,
            protocol_version: 1,
        },
        false,
    );
    assert_eq!(pong, "pong v0.1.0 (pid=4242, proto=v1)");

    let err = format_response(
        &IpcResponse::Error {
            code: IpcErrorCode::WindowProtected,
            message: "Shell".to_string(),
        },
        false,
    );
    assert_eq!(err, "error [WindowProtected]: Shell");

    let status = IpcResponse::Status(DaemonStatusSnapshot {
        pid: 4242,
        slots: vec![EdgeSlotSnapshot {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
            slot_index: 0,
            is_active_in_deck: true,
            is_open: true,
            needs_attention: false,
            dock_mode: DockMode::FloatingOverlay,
            virtual_desktop_mode: VirtualDesktopMode::AllDesktops,
            windows: vec![
                StashedWindowSnapshot {
                    hwnd: 101,
                    title: "Notepad".to_string(),
                    process_name: "notepad.exe".to_string(),
                    target_rect: Rect::new(1520, 0, 1920, 516),
                    needs_attention: false,
                },
                StashedWindowSnapshot {
                    hwnd: 102,
                    title: "Calculator".to_string(),
                    process_name: "calc.exe".to_string(),
                    target_rect: Rect::new(1560, 516, 1920, 1032),
                    needs_attention: false,
                },
            ],
        }],
    });
    let human = format_response(&status, false);
    assert!(human.contains("Notepad (101) + Calculator (102)"));

    let json = format_response(&status, true);
    assert!(json.contains("\"monitor_id\": \"MON-1\""));
}
