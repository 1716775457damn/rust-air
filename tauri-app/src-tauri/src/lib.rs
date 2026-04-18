mod commands;
mod sync_commands;
mod search_commands;
mod clip_history_commands;

use std::sync::Arc;
use tauri_plugin_dialog;
use tauri_plugin_opener;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Single Arc shared between the monitor thread and Tauri command handlers.
    let history_state = Arc::new(clip_history_commands::HistoryState::new());
    let history_for_monitor = history_state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .manage(sync_commands::SyncState::new())
        .manage(search_commands::SearchState::new())
        .manage(history_state)          // Arc<HistoryState> implements Deref<Target=HistoryState>
        .setup(move |app| {
            clip_history_commands::start_clip_monitor(app.handle().clone(), history_for_monitor);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // File transfer
            commands::start_listener,
            commands::send_to,
            commands::cancel_send,
            commands::scan_devices,
            commands::read_clipboard,
            commands::write_clipboard,
            commands::get_local_ips,
            commands::open_path,
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
            // Clipboard history
            clip_history_commands::get_history,
            clip_history_commands::copy_history_entry,
            clip_history_commands::delete_history_entry,
            clip_history_commands::toggle_pin_entry,
            clip_history_commands::clear_history,
            clip_history_commands::set_history_paused,
            clip_history_commands::flush_history,
            clip_history_commands::tick_history,
            clip_history_commands::get_history_paused,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
