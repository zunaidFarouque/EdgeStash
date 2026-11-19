use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use edgestash_ipc::pipe::{PipeClient, PipeServer};
use edgestash_ipc::protocol::{IpcRequest, IpcResponse};

static PIPE_SEQ: AtomicU64 = AtomicU64::new(1);

fn unique_pipe_name() -> String {
    let n = PIPE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!(r"\\.\pipe\EdgeStashTest-{}-{n}", std::process::id())
}

#[test]
fn named_pipe_roundtrips_multiple_requests_across_threads() {
    let pipe_name = unique_pipe_name();
    let mut server = PipeServer::bind(&pipe_name).expect("server should bind named pipe");

    let handle = thread::spawn(move || {
        // Handle two sequential client connections
        for i in 0..2 {
            server
                .accept_one(|req| match req {
                    IpcRequest::Ping => IpcResponse::Pong {
                        version: format!("0.1.0-{i}"),
                        pid: std::process::id(),
                    },
                    IpcRequest::ReloadConfig => IpcResponse::Ok {
                        message: "reloaded".to_string(),
                    },
                    other => IpcResponse::Ok {
                        message: format!("got {other:?}"),
                    },
                })
                .expect("accept_one should succeed");
        }
    });

    let client = PipeClient::new(&pipe_name);
    let r1 = client.send(&IpcRequest::Ping).expect("ping 1");
    assert_eq!(
        r1,
        IpcResponse::Pong {
            version: "0.1.0-0".to_string(),
            pid: std::process::id()
        }
    );

    let r2 = client.send(&IpcRequest::ReloadConfig).expect("reload 2");
    assert_eq!(
        r2,
        IpcResponse::Ok {
            message: "reloaded".to_string()
        }
    );

    handle.join().expect("server thread finished cleanly");
}

#[test]
fn pipe_client_returns_error_when_daemon_not_running() {
    let pipe_name = unique_pipe_name();
    let client = PipeClient::new(&pipe_name);
    let err = client.send(&IpcRequest::Ping).expect_err("no server listening");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}
