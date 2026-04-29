mod commands;
mod sync_commands;
mod search_commands;
mod clip_history_commands;
mod update_commands;
mod todo_commands;

use std::sync::{Arc, Mutex};
use tauri::Emitter;
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
        .manage(Mutex::new(todo_commands::TodoStore::new()))
        .setup(move |app| {
            clip_history_commands::start_clip_monitor(app.handle().clone(), history_for_monitor);

            // Clean up installer files left over from a previous update.
            update_commands::cleanup_old_update_files();

            // Auto-update check on startup
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let settings = update_commands::UpdateSettings::load();
                if !settings.auto_check { return; }
                // Small delay so the window is visible before any banner appears
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                match update_commands::check_update().await {
                    Ok(Some(info)) => {
                        if settings.auto_install {
                            // Silent background install
                            let _ = update_commands::download_and_install(
                                info.url, info.size, app_handle
                            ).await;
                        } else {
                            app_handle.emit("update-available", &info).ok();
                        }
                    }
                    _ => {}
                }
            });
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
            // Auto-update
            update_commands::get_update_settings,
            update_commands::save_update_settings,
            update_commands::check_update,
            update_commands::download_and_install,
            // Todo
            todo_commands::get_todos,
            todo_commands::add_todo,
            todo_commands::toggle_todo,
            todo_commands::delete_todo,
            todo_commands::get_todo_dates,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
