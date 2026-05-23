mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::read_dir_tree,
            commands::read_file,
            commands::write_file,
            commands::parse_maven_modules,
            commands::run_maven_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
