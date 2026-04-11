mod commands;

use tauri_plugin_dialog;
use tauri_plugin_opener;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::start_send,
            commands::cancel_send,
            commands::start_receive,
            commands::scan_devices,
            commands::read_clipboard,
            commands::write_clipboard,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
