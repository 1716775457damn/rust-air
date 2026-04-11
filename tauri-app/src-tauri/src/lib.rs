mod commands;
mod sync_commands;
mod search_commands;

use tauri_plugin_dialog;
use tauri_plugin_opener;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .manage(sync_commands::SyncState::new())
        .manage(search_commands::SearchState::new())
        .invoke_handler(tauri::generate_handler![
            // File transfer
            commands::start_send,
            commands::cancel_send,
            commands::start_receive,
            commands::scan_devices,
            commands::read_clipboard,
            commands::write_clipboard,
            commands::get_local_ips,
            // Sync vault
            sync_commands::get_sync_config,
            sync_commands::save_sync_config,
            sync_commands::get_sync_status,
            sync_commands::get_default_excludes,
            sync_commands::start_sync,
            sync_commands::sync_done,
            sync_commands::start_watch,
            sync_commands::stop_watch,
            // File search
            search_commands::start_search,
            search_commands::cancel_search,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
