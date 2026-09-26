//! A desktop window on the Pixel 7 while it runs Ferrix.
//!
//! It shows where the phone is (Android, fastboot, or away in Ferrix), boots
//! it into Ferrix through the launcher's helper, or runs Ferrix as a guest of
//! the phone's own crosvm with its console live. It graphs the phone's
//! processors, memory, temperatures, GPU and battery, and any crosvm's
//! processor and memory use, over time. And it keeps every run's record to
//! read back. `README.md` says how to build and run it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod helper;
mod phone;
mod runs;
mod vm;

use std::process::Child;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, State};

/// What the app holds: the guest it started, and the helper it started.
#[derive(Default)]
struct App {
    guest: Arc<Mutex<Option<Child>>>,
    helper: Mutex<Option<Child>>,
}

#[tauri::command]
fn boot_native() -> Result<serde_json::Value, String> {
    helper::boot()
}

#[tauri::command]
fn start_helper(app: State<'_, App>, image: Option<String>) -> Result<(), String> {
    let mut helper = app.helper.lock().map_err(|_| "poisoned")?;
    if let Some(child) = helper.as_mut()
        && child.try_wait().ok().flatten().is_none()
    {
        return Ok(());
    }
    *helper = Some(helper::start(image)?);
    Ok(())
}

#[tauri::command]
fn vm_start(handle: AppHandle, app: State<'_, App>, cpus: u32, memory: u32) -> Result<(), String> {
    vm::start(handle, app.guest.clone(), cpus.clamp(1, 8), memory.clamp(256, 6144))
}

#[tauri::command]
fn vm_control(command: String) -> Result<(), String> {
    vm::control(&command)
}

#[tauri::command]
fn list_runs() -> Vec<runs::Run> {
    runs::list()
}

#[tauri::command]
fn read_run(path: String) -> Result<String, String> {
    runs::read(&path)
}

fn main() {
    tauri::Builder::default()
        .manage(App::default())
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || phone::poll(handle));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let app = window.state::<App>();
                if let Ok(mut helper) = app.helper.lock()
                    && let Some(mut child) = helper.take()
                {
                    let _ = child.kill();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            boot_native,
            start_helper,
            vm_start,
            vm_control,
            list_runs,
            read_run
        ])
        .run(tauri::generate_context!())
        .expect("the window could not be opened");
}
