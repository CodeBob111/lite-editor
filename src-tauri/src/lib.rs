mod commands;
mod lsp;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(lsp::LspState::default())
        .invoke_handler(tauri::generate_handler![
            commands::read_dir_tree,
            commands::read_file,
            commands::write_file,
            commands::parse_maven_modules,
            commands::run_maven_command,
            lsp::start_lsp,
            lsp::stop_lsp,
            lsp::lsp_did_open,
            lsp::lsp_did_change,
            lsp::lsp_find_references,
            lsp::lsp_goto_definition,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
