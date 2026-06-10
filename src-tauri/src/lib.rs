// 过渡期旧 Nib(Tauri 壳):业务逻辑全部在 nib-core,本 crate 只剩
// 命令薄壳(commands.rs)、终端(terminal.rs,M4 迁拉模型)、菜单与生命周期。

pub mod commands;
mod terminal;

use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{Emitter, Manager};

// core 事件 → Tauri emit:事件名/载荷与历史前端完全一致
struct TauriSink(tauri::AppHandle);

impl nib_core::EventSink for TauriSink {
    fn emit(&self, event: nib_core::CoreEvent) {
        use nib_core::CoreEvent::*;
        match event {
            FileChanged {
                project,
                has_structural,
            } => {
                let _ = self.0.emit(
                    "file-changed",
                    serde_json::json!({ "project": project, "hasStructural": has_structural }),
                );
            }
            LspDiagnostics(params) => {
                let _ = self.0.emit("lsp-diagnostics", params);
            }
            LspProgress {
                language,
                kind,
                message,
                percentage,
            } => {
                let _ = self.0.emit(
                    "lsp-progress",
                    serde_json::json!({
                        "language": language,
                        "kind": kind,
                        "message": message,
                        "percentage": percentage,
                    }),
                );
            }
            MavenOutput(line) => {
                let _ = self.0.emit("maven-output", line);
            }
            MavenDone(code) => {
                let _ = self.0.emit("maven-done", code);
            }
        }
    }
}

// 旧 Nib 的数据路径与历史完全一致(session/settings 在 Tauri app_data_dir,
// 缓存在 ~/Library/Caches/lite-editor,astore 会话在 data_local_dir/lite-editor)。
// 原生 nib-app 走独立目录(critic V11),互不相干。
fn legacy_dirs(app: &tauri::App) -> tauri::Result<nib_core::DataDirs> {
    let app_data = app.path().app_data_dir()?;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let cache = std::path::PathBuf::from(format!("{}/Library/Caches/lite-editor", home));
    let astore_session = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("lite-editor")
        .join("astore-session.json");
    Ok(nib_core::DataDirs {
        app_data,
        cache,
        astore_session,
    })
}

fn build_menu(app: &tauri::App) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let app_menu = SubmenuBuilder::new(app, "Nib")
        .item(&PredefinedMenuItem::about(
            app,
            Some("About Nib"),
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
            let dirs = legacy_dirs(app)?;
            let sink = Arc::new(TauriSink(app.handle().clone()));
            app.manage(Arc::new(nib_core::NibCore::new(sink, dirs)));
            Ok(())
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            let _ = app.emit("menu-action", id);
        })
        .manage(terminal::TerminalState::default())
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
            commands::render_markdown,
            commands::maven_dependency_tree,
            commands::maven_add_exclusion,
            commands::start_file_watcher,
            commands::stop_file_watcher,
            commands::save_session,
            commands::load_session,
            commands::save_settings,
            commands::load_settings,
            commands::copy_files_to_clipboard,
            commands::copy_text_to_clipboard,
            commands::start_lsp,
            commands::stop_lsp,
            commands::lsp_did_open,
            commands::lsp_did_change,
            commands::lsp_find_references,
            commands::lsp_goto_definition,
            commands::lsp_document_symbols,
            commands::find_class_in_maven,
            commands::lsp_is_ready,
            terminal::spawn_terminal,
            terminal::write_terminal,
            terminal::resize_terminal,
            terminal::close_terminal,
            terminal::get_claude_status,
            commands::git_current_branch,
            commands::git_list_branches,
            commands::git_checkout,
            commands::git_new_branch,
            commands::git_delete_branch,
            commands::git_prune,
            commands::git_pull,
            commands::git_fetch_branch,
            commands::git_push,
            commands::git_merge,
            commands::git_merge_conflicts,
            commands::parse_conflict_file,
            commands::git_show_conflict_version,
            commands::git_merge_abort,
            commands::git_resolve_conflict_file,
            commands::git_checkout_conflict_side,
            commands::git_discard_changes,
            commands::git_rebase,
            commands::git_rename_branch,
            commands::git_log,
            commands::git_commit_patch,
            commands::git_cherry_pick,
            commands::git_checkout_revision,
            commands::git_revert_commit,
            commands::git_new_branch_at_commit,
            commands::git_commit,
            commands::git_status,
            commands::git_show_file,
            commands::git_show_staged,
            commands::git_discover_repos,
            commands::git_blame,
            commands::git_clone,
            commands::git_remote_url,
            commands::git_status_batch,
            commands::git_current_branch_batch,
            commands::build_java_index,
            commands::load_java_index,
            commands::search_java_class,
            commands::update_java_index_file,
            commands::remove_java_index_file,
            commands::build_usage_index,
            commands::query_usages,
            commands::update_usage_index_file,
            commands::remove_usage_index_file,
            commands::astore_login,
            commands::astore_logout,
            commands::astore_get_session,
            commands::astore_detect_project,
            commands::astore_set_env,
            commands::astore_sync_code,
            commands::astore_publish,
            commands::astore_get_editor_url,
            commands::astore_get_record_url,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Tauri 在事件循环结束后直接 process::exit,托管状态的 Drop 不执行——
            // 必须在 Exit 事件里显式杀掉 LSP 子进程,否则 jdtls(每个 ~1.5G 堆)
            // 在每次退出后变孤儿、跨重启累积吃光内存。
            if let tauri::RunEvent::Exit = event {
                if let Some(core) = app_handle.try_state::<Arc<nib_core::NibCore>>() {
                    core.shutdown();
                }
            }
        });
}
