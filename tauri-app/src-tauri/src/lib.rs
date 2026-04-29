mod commands;
#[cfg(feature = "desktop")]
mod sync_commands;
#[cfg(feature = "desktop")]
mod search_commands;
#[cfg(feature = "desktop")]
mod clip_history_commands;
#[cfg(feature = "desktop")]
mod clip_sync_commands;
#[cfg(feature = "desktop")]
mod update_commands;
mod todo_commands;

use std::sync::Mutex;
#[cfg(feature = "desktop")]
use std::sync::Arc;
#[cfg(feature = "desktop")]
use tauri::Emitter;
use tauri_plugin_dialog;
use tauri_plugin_opener;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .manage(Mutex::new(todo_commands::TodoStore::new()));

    #[cfg(feature = "desktop")]
    {
        // Single Arc shared between the monitor thread and Tauri command handlers.
        let history_state = Arc::new(clip_history_commands::HistoryState::new());
        let history_for_monitor = history_state.clone();

        // Clipboard sync service — shared between commands, monitor, and receiver.
        let sync_service = Arc::new(rust_air_core::clipboard_sync::ClipboardSyncService::new());
        let clip_sync_state = clip_sync_commands::ClipSyncState {
            service: sync_service.clone(),
        };
        let sync_for_monitor = sync_service.clone();
        let sync_for_peer_check = sync_service.clone();

        builder = builder
            .manage(sync_commands::SyncState::new())
            .manage(search_commands::SearchState::new())
            .manage(history_state)
            .manage(clip_sync_state);

        builder = builder.setup(move |app| {
            clip_history_commands::start_clip_monitor(
                app.handle().clone(),
                history_for_monitor,
                sync_for_monitor,
            );

            // Periodic peer online check — every 30 seconds, mark stale peers offline.
            let peer_check_service = sync_for_peer_check.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let mut cfg = peer_check_service.config();
                    let mut changed = false;
                    for peer in &mut cfg.peers {
                        if peer.online && now.saturating_sub(peer.last_seen) > 30 {
                            peer.online = false;
                            changed = true;
                        }
                    }
                    if changed {
                        peer_check_service.save_config(cfg);
                    }
                }
            });

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
        });
    }

    #[cfg(not(feature = "desktop"))]
    {
        builder = builder.setup(move |_app| {
            Ok(())
        });
    }

    // Command registration: desktop version registers all commands,
    // non-desktop version only registers core transfer + todo commands.
    #[cfg(feature = "desktop")]
    {
        builder = builder.invoke_handler(tauri::generate_handler![
            // File transfer
            commands::start_listener,
            commands::send_to,
            commands::cancel_send,
            commands::retry_send,
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
            // Clipboard sync
            clip_sync_commands::get_sync_group,
            clip_sync_commands::save_sync_group,
            clip_sync_commands::add_sync_peer,
            clip_sync_commands::remove_sync_peer,
            clip_sync_commands::set_clip_sync_enabled,
            clip_sync_commands::get_clip_sync_enabled,
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
        ]);
    }

    #[cfg(not(feature = "desktop"))]
    {
        builder = builder.invoke_handler(tauri::generate_handler![
            // Core file transfer (always available)
            commands::start_listener,
            commands::send_to,
            commands::cancel_send,
            commands::retry_send,
            commands::scan_devices,
            commands::get_local_ips,
            // Todo (always available)
            todo_commands::get_todos,
            todo_commands::add_todo,
            todo_commands::toggle_todo,
            todo_commands::delete_todo,
            todo_commands::get_todo_dates,
        ]);
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
