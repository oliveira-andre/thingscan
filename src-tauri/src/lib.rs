mod account;
mod commands;
mod store;
mod sync;
mod tray;
mod vault;

use std::path::PathBuf;
use tauri::{Manager, RunEvent, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // THINGSCAN_DATA_DIR points a second copy at scratch data, leaving the real home alone.
            let dir = match std::env::var_os("THINGSCAN_DATA_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => app.path().app_data_dir()?,
            };
            app.manage(store::Store::open(&dir).map_err(|e| e.to_string())?);
            app.manage(account::Account::open(&dir));
            app.manage(sync::Sync::open(&dir));
            app.manage(vault::Vault::open(&dir));
            app.manage(commands::ScanTask::default());
            app.manage(commands::TvSessions::default());
            app.manage(commands::Reports::default());
            app.manage(tray::PanelState::default());
            tray::setup(app.handle())?;
            sync::start(app.handle().clone());
            vault::start(app.handle().clone());
            account::start(app.handle().clone());
            Ok(())
        })
        // Closing the main window keeps Thingscan in the menu bar; Quit or Cmd+Q exits.
        .on_window_event(|window, event| {
            if let (tray::MAIN, WindowEvent::CloseRequested { api, .. }) = (window.label(), event) {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_devices,
            commands::add_device,
            commands::update_device,
            commands::rename_room,
            commands::remove_device,
            commands::start_scan,
            commands::cancel_scan,
            commands::check_devices,
            commands::last_reports,
            commands::open_main,
            commands::cloud_status,
            commands::cloud_connect,
            commands::cloud_disconnect,
            commands::cloud_token,
            commands::cloud_set_app,
            commands::cloud_sign_in,
            commands::cloud_link,
            commands::cloud_add,
            commands::cloud_device_status,
            commands::cloud_command,
            commands::tv_connect,
            commands::tv_disconnect,
            commands::tv_key,
            commands::tv_press,
            commands::tv_release,
            commands::tv_launch,
            commands::tv_wake,
            commands::tv_pointer,
            commands::tv_pointer_click,
            commands::sync_status,
            commands::sync_start_pairing,
            commands::sync_cancel_pairing,
            commands::sync_remove_phone,
            commands::account_status,
            commands::account_sign_up,
            commands::account_sign_in,
            commands::account_sign_out,
            commands::account_sync_now,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Thingscan")
        .run(|app, event| {
            // Clicking the Dock icon brings back a hidden main window.
            if let RunEvent::Reopen { .. } = event {
                tray::show_main(app, None);
            }
        });
}
