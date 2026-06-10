// 会话持久化:schema 与旧 Nib(webview 版 state.ts PersistedSession)完全一致,
// 字段名沿用 camelCase——一次性导入因此退化成「整文件拷贝」。
// 数据目录独立于旧 app(RFC v2 critic V11:并存期零共享可变文件)。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct PersistedSession {
    pub version: u32,
    pub projects: Vec<ProjectSession>,
    #[serde(rename = "activeProjectIndex")]
    pub active_project_index: usize,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ProjectSession {
    pub path: String,
    #[serde(rename = "openFiles")]
    pub open_files: Vec<String>,
    #[serde(rename = "activeFile")]
    pub active_file: Option<String>,
}

pub fn data_dirs() -> nib_core::DataDirs {
    let app_data = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nib");
    let cache = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nib");
    nib_core::DataDirs {
        astore_session: app_data.join("astore-session.json"),
        app_data,
        cache,
    }
}

/// 旧 Nib(Tauri)的数据目录,只读不写
fn legacy_app_data() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.lite-editor.app")
}

/// 读会话;本地没有时从旧 Nib 一次性导入(schema 相同,原样拷贝)
pub async fn load() -> Option<PersistedSession> {
    let dirs = data_dirs();
    let raw = match nib_core::session::load_session(&dirs).await.ok()? {
        Some(raw) => raw,
        None => {
            let legacy = legacy_app_data();
            let imported = nib_core::rt::run(async move {
                let session = std::fs::read_to_string(legacy.join("session.json")).ok();
                let settings = std::fs::read_to_string(legacy.join("settings.json")).ok();
                (session, settings)
            })
            .await;
            let (session, settings) = imported;
            if let Some(s) = &settings {
                let _ = nib_core::session::save_settings(&dirs, s.clone()).await;
            }
            let session = session?;
            let _ = nib_core::session::save_session(&dirs, session.clone()).await;
            session
        }
    };
    serde_json::from_str(&raw).ok()
}

pub fn save(session: &PersistedSession) {
    if let Ok(json) = serde_json::to_string(session) {
        let dirs = data_dirs();
        nib_core::rt::detach(async move {
            let _ = nib_core::session::save_session(&dirs, json).await;
        });
    }
}

// ---- 编辑器偏好(沿用旧 Nib settings.json 的扁平键 schema) ----

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct EditorSettings {
    #[serde(rename = "editor.fontSize", default = "default_font_size")]
    pub font_size: f32,
    #[serde(rename = "editor.tabSize", default = "default_tab_size")]
    pub tab_size: u32,
    #[serde(rename = "editor.wordWrap", default)]
    pub word_wrap: bool,
    #[serde(rename = "editor.folding", default = "default_true")]
    pub folding: bool,
}

fn default_font_size() -> f32 {
    13.0
}
fn default_tab_size() -> u32 {
    4
}
fn default_true() -> bool {
    true
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            tab_size: default_tab_size(),
            word_wrap: false,
            folding: true,
        }
    }
}

/// 读偏好(load() 的一次性导入已把旧 settings.json 搬进本地目录)
pub async fn load_settings() -> EditorSettings {
    let dirs = data_dirs();
    match nib_core::session::load_settings(&dirs).await {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        _ => EditorSettings::default(),
    }
}

/// 写偏好(同一扁平键 schema,设置浮层保存时调;fire-and-forget)
pub fn save_settings(settings: EditorSettings) {
    nib_core::rt::detach(async move {
        let dirs = data_dirs();
        if let Ok(raw) = serde_json::to_string_pretty(&settings) {
            let _ = nib_core::session::save_settings(&dirs, raw).await;
        }
    });
}

// ---- 最近项目(对齐旧版 recent-projects:上限 12,最近优先) ----
// 旧版存 webview localStorage,原生版无法导入,从空列表重新积累(档案已声明)。

const RECENTS_MAX: usize = 12;

fn recents_path() -> PathBuf {
    data_dirs().app_data.join("recent-projects.json")
}

pub async fn load_recents() -> Vec<String> {
    nib_core::rt::run(async move {
        std::fs::read_to_string(recents_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default()
    })
    .await
}

/// 项目装载成功后调(fire-and-forget,core runtime 落盘)
pub fn remember_recent(path: String) {
    nib_core::rt::detach(async move {
        let file = recents_path();
        let mut list: Vec<String> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        list.retain(|p| p != &path);
        list.insert(0, path);
        list.truncate(RECENTS_MAX);
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = std::fs::write(&file, json);
        }
    });
}
