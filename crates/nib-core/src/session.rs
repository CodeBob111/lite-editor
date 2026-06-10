// session.json / settings.json 持久化(自 commands.rs 迁入)。
// 目录由宿主注入(DataDirs::app_data):旧 Nib 传 Tauri app_data_dir 路径不变;
// 原生 app 用独立目录 + 首启一次性导入。前端/调用方拿原始 JSON 字符串自己解析,
// 后端只负责存取(透传契约不变)。

use crate::paths::DataDirs;
use crate::rt::on_worker;
use std::path::PathBuf;

async fn save_json(dir: PathBuf, file_name: &'static str, data: String) -> Result<(), String> {
    on_worker(move || {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create app data dir: {}", e))?;
        std::fs::write(dir.join(file_name), data)
            .map_err(|e| format!("Failed to save {}: {}", file_name, e))
    })
    .await
}

async fn load_json(dir: PathBuf, file_name: &'static str) -> Result<Option<String>, String> {
    on_worker(move || {
        let file = dir.join(file_name);
        if file.exists() {
            let data = std::fs::read_to_string(&file)
                .map_err(|e| format!("Failed to read {}: {}", file_name, e))?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    })
    .await
}

pub async fn save_session(dirs: &DataDirs, data: String) -> Result<(), String> {
    save_json(dirs.app_data.clone(), "session.json", data).await
}

pub async fn load_session(dirs: &DataDirs) -> Result<Option<String>, String> {
    load_json(dirs.app_data.clone(), "session.json").await
}

pub async fn save_settings(dirs: &DataDirs, data: String) -> Result<(), String> {
    save_json(dirs.app_data.clone(), "settings.json", data).await
}

pub async fn load_settings(dirs: &DataDirs) -> Result<Option<String>, String> {
    load_json(dirs.app_data.clone(), "settings.json").await
}
