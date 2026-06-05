mod astore;
pub mod clipboard;
mod commands;
pub mod git;
mod java_index;
mod lsp;
mod terminal;

use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::Emitter;

fn build_menu(app: &tauri::App) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let app_menu = SubmenuBuilder::new(app, "Lite Editor")
        .item(&PredefinedMenuItem::about(
            app,
            Some("About Lite Editor"),
            None,
        )?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, None)?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .item(&PredefinedMenuItem::show_all(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, None)?)
        .build()?;

    let file_menu = SubmenuBuilder::new(app, "File")
        .item(
            &MenuItemBuilder::with_id("open-folder", "Open Folder...")
                .accelerator("CmdOrCtrl+O")
                .build(app)?,
        )
        .item(&MenuItemBuilder::with_id("recent-projects", "Recent Projects...").build(app)?)
        .item(&MenuItemBuilder::with_id("vcs-clone", "Get from Version Control...").build(app)?)
        .item(
            &MenuItemBuilder::with_id("save", "Save")
                .accelerator("CmdOrCtrl+S")
                .build(app)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("close-tab", "Close Tab")
                .accelerator("CmdOrCtrl+W")
                .build(app)?,
        )
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .item(&PredefinedMenuItem::undo(app, None)?)
        .item(&PredefinedMenuItem::redo(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, None)?)
        .item(&PredefinedMenuItem::copy(app, None)?)
        .item(&PredefinedMenuItem::paste(app, None)?)
        .item(&PredefinedMenuItem::select_all(app, None)?)
        .separator()
        .item(
            &MenuItemBuilder::with_id("find-in-files", "Find in Files...")
                .accelerator("CmdOrCtrl+Shift+F")
                .build(app)?,
        )
        .build()?;

    let go_menu = SubmenuBuilder::new(app, "Go")
        .item(&MenuItemBuilder::with_id("quick-open", "Quick Open File...").build(app)?)
        .item(
            &MenuItemBuilder::with_id("goto-definition", "Go to Definition")
                .accelerator("F12")
                .build(app)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("navigate-back", "Back")
                .accelerator("CmdOrCtrl+[")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("navigate-forward", "Forward")
                .accelerator("CmdOrCtrl+]")
                .build(app)?,
        )
        .build()?;

    let view_menu = SubmenuBuilder::new(app, "View")
        .item(&MenuItemBuilder::with_id("toggle-terminal", "Terminal").build(app)?)
        .item(&MenuItemBuilder::with_id("toggle-git", "Git").build(app)?)
        .item(&MenuItemBuilder::with_id("toggle-astore", "Astore").build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("export-perf", "Export Perf Report").build(app)?)
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::fullscreen(app, None)?)
        .build()?;

    MenuBuilder::new(app)
        .items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &go_menu,
            &view_menu,
            &window_menu,
        ])
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let menu = build_menu(app)?;
            app.set_menu(menu)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            let _ = app.emit("menu-action", id);
        })
        .manage(lsp::LspState::default())
        .manage(commands::WatcherState::default())
        .manage(terminal::TerminalState::default())
        .manage(java_index::JavaIndexState::default())
        .manage(astore::AstoreState::default())
        .invoke_handler(tauri::generate_handler![
            commands::read_dir_tree,
            commands::read_file,
            commands::write_file,
            commands::create_file,
            commands::create_dir,
            commands::delete_path,
            commands::copy_path,
            commands::rename_path,
            commands::search_in_files,
            commands::list_all_files,
            commands::parse_maven_modules,
            commands::run_maven_command,
            commands::run_maven_collect,
            commands::start_file_watcher,
            commands::stop_file_watcher,
            commands::save_session,
            commands::load_session,
            clipboard::copy_files_to_clipboard,
            lsp::start_lsp,
            lsp::stop_lsp,
            lsp::lsp_did_open,
            lsp::lsp_did_change,
            lsp::lsp_find_references,
            lsp::lsp_goto_definition,
            lsp::find_class_in_maven,
            lsp::lsp_is_ready,
            terminal::spawn_terminal,
            terminal::write_terminal,
            terminal::resize_terminal,
            terminal::close_terminal,
            terminal::get_claude_status,
            git::git_current_branch,
            git::git_list_branches,
            git::git_checkout,
            git::git_new_branch,
            git::git_delete_branch,
            git::git_prune,
            git::git_pull,
            git::git_fetch_branch,
            git::git_push,
            git::git_merge,
            git::git_merge_conflicts,
            git::git_show_conflict_version,
            git::git_merge_abort,
            git::git_resolve_conflict_file,
            git::git_checkout_conflict_side,
            git::git_discard_changes,
            git::git_rebase,
            git::git_rename_branch,
            git::git_log,
            git::git_commit_patch,
            git::git_cherry_pick,
            git::git_checkout_revision,
            git::git_revert_commit,
            git::git_new_branch_at_commit,
            git::git_commit,
            git::git_status,
            git::git_show_file,
            git::git_show_staged,
            git::git_discover_repos,
            git::git_blame,
            git::git_clone,
            git::git_remote_url,
            git::git_status_batch,
            git::git_current_branch_batch,
            java_index::build_java_index,
            java_index::load_java_index,
            java_index::search_java_class,
            java_index::update_java_index_file,
            java_index::remove_java_index_file,
            astore::astore_login,
            astore::astore_logout,
            astore::astore_get_session,
            astore::astore_detect_project,
            astore::astore_set_env,
            astore::astore_sync_code,
            astore::astore_publish,
            astore::astore_get_editor_url,
            astore::astore_get_record_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
