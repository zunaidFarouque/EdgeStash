#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match edgestash_daemon::daemon::run_daemon() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("edgestash-daemon error: {err}");
            ExitCode::FAILURE
        }
    }
}
