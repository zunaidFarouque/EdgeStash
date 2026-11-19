#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;
use std::sync::Arc;

use edgestash_ui::app::{EdgeStashApp, resolve_default_config_path};
use edgestash_ui::ipc_bridge::bring_existing_window_to_front;
use edgestash_ui::model::EdgeStashUiModel;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

const WINDOW_TITLE: &str = "EdgeStash Settings";

struct MutexGuard(HANDLE);

impl Drop for MutexGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn build_app_icon() -> egui::IconData {
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let idx = (y * size + x) * 4;
            let in_box = (2..30).contains(&x) && (2..30).contains(&y);
            if !in_box {
                continue;
            }
            // Right-edge glowing pill + dark card interior
            let is_pill = (22..27).contains(&x) && (7..25).contains(&y);
            let is_drawer = (8..20).contains(&x) && (6..26).contains(&y);
            let is_border = x == 2 || x == 29 || y == 2 || y == 29;
            let (r, g, b, a) = if is_pill {
                (76, 194, 255, 255)
            } else if is_drawer {
                (44, 54, 74, 255)
            } else if is_border {
                (66, 77, 100, 255)
            } else {
                (20, 25, 34, 255)
            };
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = a;
        }
    }
    egui::IconData {
        rgba,
        width: size as u32,
        height: size as u32,
    }
}

fn main() -> ExitCode {
    let _instance_guard = unsafe {
        match CreateMutexW(None, false, w!("Local\\EdgeStashSettingsMutex")) {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let _ = bring_existing_window_to_front(WINDOW_TITLE);
                    let _ = CloseHandle(h);
                    return ExitCode::SUCCESS;
                }
                Some(MutexGuard(h))
            }
            Err(_) => None,
        }
    };

    let config_path = resolve_default_config_path();
    let model = match EdgeStashUiModel::load_or_create(config_path) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("edgestash-ui config error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([740.0, 520.0])
            .with_icon(Arc::new(build_app_icon())),
        ..Default::default()
    };

    match eframe::run_native(
        WINDOW_TITLE,
        native_options,
        Box::new(|cc| Ok(Box::new(EdgeStashApp::new(cc, model)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("edgestash-ui error: {err}");
            ExitCode::FAILURE
        }
    }
}
