mod commands;
mod clip_history_commands;
mod sync_commands;

use tauri_plugin_dialog;
use tauri_plugin_opener;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .manage(clip_history_commands::HistoryState::new())
        .manage(sync_commands::SyncState::new())
        .invoke_handler(tauri::generate_handler![
            // File transfer
            commands::start_send,
            commands::cancel_send,
            commands::start_receive,
            commands::scan_devices,
            commands::read_clipboard,
            commands::write_clipboard,
            // Clipboard history
            clip_history_commands::tick_history,
            clip_history_commands::get_history,
            clip_history_commands::copy_history_entry,
            clip_history_commands::delete_history_entry,
            clip_history_commands::toggle_pin_entry,
            clip_history_commands::clear_history,
            clip_history_commands::set_history_paused,
            clip_history_commands::get_history_paused,
            clip_history_commands::flush_history,
            // Sync vault
            sync_commands::get_sync_config,
            sync_commands::save_sync_config,
            sync_commands::get_sync_status,
            sync_commands::get_default_excludes,
            sync_commands::start_sync,
            sync_commands::sync_done,
            sync_commands::start_watch,
            sync_commands::stop_watch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
