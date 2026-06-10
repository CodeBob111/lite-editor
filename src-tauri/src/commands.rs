use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, State};
use walkdir::WalkDir;

// ---- File watcher state ----

#[derive(Serialize, Clone)]
struct FileChangeEvent {
    project: String,
    #[serde(rename = "hasStructural")]
    has_structural: bool,
}

#[derive(Default)]
pub struct WatcherState {
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
}

// ---- Worker helper ----

// 同步 Tauri 命令在主线程执行,任何磁盘/子进程等待都会冻住整个 UI。
// 所有做阻塞 IO 的命令统一经这里搬到阻塞线程池。
pub(crate) async fn on_worker<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

// ---- File tree ----

#[derive(Serialize, Clone)]
pub struct FileNode {
    name: String,
    path: String,
    #[serde(rename = "isDir")]
    is_dir: bool,
    children: Option<Vec<FileNode>>,
}

#[tauri::command]
pub async fn read_dir_tree(path: String, max_depth: Option<usize>) -> Result<FileNode, String> {
    on_worker(move || {
        let root = Path::new(&path);
        if !root.exists() {
            return Err(format!("Path does not exist: {}", path));
        }
        build_tree(root, max_depth.unwrap_or(4), 0)
            .ok_or_else(|| "Failed to read directory".into())
    })
    .await
}

fn build_tree(path: &Path, max_depth: usize, current_depth: usize) -> Option<FileNode> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if should_skip(&name) {
        return None;
    }

    if path.is_dir() {
        let children = if current_depth < max_depth {
            let mut entries: Vec<FileNode> = std::fs::read_dir(path)
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| build_tree(&e.path(), max_depth, current_depth + 1))
                .collect();
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
            Some(entries)
        } else {
            Some(vec![])
        };

        Some(FileNode {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: true,
            children,
        })
    } else {
        Some(FileNode {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: false,
            children: None,
        })
    }
}

fn should_skip(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".idea" | ".vscode" | "node_modules" | "target" | ".DS_Store" | "__pycache__"
            // 构建/产物目录:否则会搜到已删代码在编译/打包输出里的陈旧副本。
            | "build" | "dist" | "out" | "bin" | ".gradle" | ".next" | ".nuxt" | ".turbo"
            | "coverage" | ".settings" | ".metadata"
    )
}

fn is_binary_ext(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    [
        ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".woff", ".woff2", ".ttf",
        ".eot", ".otf", ".zip", ".tar", ".gz", ".bz2", ".7z", ".rar", ".xz", ".pdf", ".doc",
        ".docx", ".xls", ".xlsx", ".pptx", ".exe", ".dll", ".so", ".dylib", ".class", ".jar",
        ".war", ".o", ".a", ".lib", ".pyc", ".pyo", ".mp3", ".mp4", ".avi", ".mov", ".wav",
        ".flac", ".mkv", ".lock", ".min.js", ".min.css",
    ]
    .iter()
    .any(|ext| name_lower.ends_with(ext))
}

// ---- File system commands ----

#[tauri::command]
pub async fn read_file(path: String) -> Result<String, String> {
    on_worker(move || {
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path, e))
    })
    .await
}

#[tauri::command]
pub async fn write_file(path: String, content: String) -> Result<(), String> {
    on_worker(move || {
        std::fs::write(&path, &content).map_err(|e| format!("Failed to write {}: {}", path, e))
    })
    .await
}

#[tauri::command]
pub async fn create_file(path: String) -> Result<(), String> {
    on_worker(move || {
        let p = Path::new(&path);
        if p.exists() {
            return Err(format!("Already exists: {}", path));
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent dirs: {}", e))?;
        }
        std::fs::File::create(&path).map_err(|e| format!("Failed to create {}: {}", path, e))?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn create_dir(path: String) -> Result<(), String> {
    on_worker(move || {
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("Failed to create directory {}: {}", path, e))
    })
    .await
}

#[tauri::command]
pub async fn delete_path(path: String) -> Result<(), String> {
    on_worker(move || {
        let p = Path::new(&path);
        if !p.exists() {
            return Err(format!("Path does not exist: {}", path));
        }
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| format!("Failed to delete {}: {}", path, e))
        } else {
            std::fs::remove_file(&path).map_err(|e| format!("Failed to delete {}: {}", path, e))
        }
    })
    .await
}

#[tauri::command]
pub async fn copy_path(src: String, dest: String) -> Result<(), String> {
    on_worker(move || {
        let s = Path::new(&src);
        if !s.exists() {
            return Err(format!("Source does not exist: {}", src));
        }
        if Path::new(&dest).exists() {
            return Err(format!("Destination already exists: {}", dest));
        }
        if s.is_dir() {
            copy_dir_recursive(&src, &dest)
        } else {
            if let Some(parent) = Path::new(&dest).parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create dir {}: {}", parent.display(), e))?;
            }
            std::fs::copy(&src, &dest)
                .map(|_| ())
                .map_err(|e| format!("Failed to copy {}: {}", src, e))
        }
    })
    .await
}

fn copy_dir_recursive(src: &str, dest: &str) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("Failed to create dir {}: {}", dest, e))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("Failed to read dir {}: {}", src, e))? {
        let entry = entry.map_err(|e| format!("Dir entry error: {}", e))?;
        let src_path = entry.path();
        let dest_path = Path::new(dest).join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(
                src_path.to_str().unwrap_or(""),
                dest_path.to_str().unwrap_or(""),
            )?;
        } else {
            std::fs::copy(&src_path, &dest_path)
                .map_err(|e| format!("Failed to copy {:?}: {}", src_path, e))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    on_worker(move || {
        if !Path::new(&old_path).exists() {
            return Err(format!("Path does not exist: {}", old_path));
        }
        if Path::new(&new_path).exists() {
            return Err(format!("Target already exists: {}", new_path));
        }
        std::fs::rename(&old_path, &new_path).map_err(|e| format!("Failed to rename: {}", e))
    })
    .await
}

// ---- Search (parallelized with rayon) ----

#[derive(Serialize, Clone)]
pub struct SearchResult {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub text: String,
}

#[tauri::command]
pub async fn search_in_files(
    project_path: String,
    query: String,
    case_sensitive: Option<bool>,
    max_results: Option<usize>,
) -> Result<Vec<SearchResult>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    tokio::task::spawn_blocking(move || {
        let max = max_results.unwrap_or(500);
        let case_sensitive = case_sensitive.unwrap_or(false);
        let query_cmp = if case_sensitive {
            query.clone()
        } else {
            query.to_lowercase()
        };

        let files: Vec<PathBuf> = WalkDir::new(&project_path)
            .into_iter()
            .filter_entry(|e| !should_skip(&e.file_name().to_string_lossy()))
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file() && !is_binary_ext(&e.file_name().to_string_lossy()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let found_count = AtomicU64::new(0);

        let mut results: Vec<SearchResult> = files
            .par_iter()
            .flat_map(|path| {
                if found_count.load(Ordering::Relaxed) >= max as u64 {
                    return Vec::new();
                }

                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(_) => return Vec::new(),
                };

                let mut file_results = Vec::new();
                for (i, line_text) in content.lines().enumerate() {
                    if found_count.load(Ordering::Relaxed) >= max as u64 {
                        break;
                    }
                    let haystack = if case_sensitive {
                        line_text.to_string()
                    } else {
                        line_text.to_lowercase()
                    };
                    if let Some(col) = haystack.find(&query_cmp) {
                        found_count.fetch_add(1, Ordering::Relaxed);
                        file_results.push(SearchResult {
                            path: path.to_string_lossy().to_string(),
                            line: i as u32,
                            column: col as u32,
                            text: line_text.trim().to_string(),
                        });
                    }
                }
                file_results
            })
            .collect();

        results.truncate(max);
        Ok(results)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn list_all_files(project_path: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for entry in WalkDir::new(&project_path)
            .into_iter()
            .filter_entry(|e| !should_skip(&e.file_name().to_string_lossy()))
            .filter_map(|e| e.ok())
        {
            if entry.path().is_file() {
                files.push(entry.path().to_string_lossy().to_string());
            }
        }
        Ok(files)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

// ---- File watcher ----

#[tauri::command]
pub fn start_file_watcher(
    project_path: String,
    app: tauri::AppHandle,
    state: State<'_, WatcherState>,
) -> Result<(), String> {
    let mut watchers = state.watchers.lock().map_err(|e| e.to_string())?;

    if watchers.contains_key(&project_path) {
        return Ok(());
    }

    let app_handle = app.clone();
    let path_clone = project_path.clone();
    let last_emit_ms = Arc::new(AtomicU64::new(0));
    let last_clone = last_emit_ms.clone();
    let structural_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let structural_clone = structural_flag.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // 忽略构建产物/依赖/VCS 目录(target、.git、node_modules、.metadata 等)的事件:
            // 新项目打开时 jdtls 会触发 Maven 构建,往 target/ 写大量 .class。否则这些事件每
            // 500ms 就触发一次整树刷新(readDirTree + 展开态全树遍历),索引期间界面持续卡顿。
            // 按项目根的相对路径判断,避免祖先目录恰好叫 out/bin 之类时全部误伤。
            if event.paths.iter().all(|p| {
                p.strip_prefix(&path_clone)
                    .map(|rel| rel.components().any(|c| {
                        c.as_os_str().to_str().map(should_skip).unwrap_or(false)
                    }))
                    .unwrap_or(false)
            }) {
                return;
            }
            match event.kind {
                EventKind::Create(_) | EventKind::Remove(_) => {
                    structural_clone.store(true, Ordering::Relaxed);
                }
                EventKind::Modify(_) => {}
                _ => return,
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let last = last_clone.load(Ordering::Relaxed);
            if now.saturating_sub(last) > 500 {
                last_clone.store(now, Ordering::Relaxed);
                let has_structural = structural_clone.swap(false, Ordering::Relaxed);
                let _ = app_handle.emit("file-changed", FileChangeEvent {
                    project: path_clone.clone(),
                    has_structural,
                });
            }
        }
    })
    .map_err(|e| format!("Failed to create watcher: {}", e))?;

    watcher
        .watch(Path::new(&project_path), RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch {}: {}", project_path, e))?;

    watchers.insert(project_path, watcher);
    Ok(())
}

#[tauri::command]
pub fn stop_file_watcher(
    project_path: String,
    state: State<'_, WatcherState>,
) -> Result<(), String> {
    let mut watchers = state.watchers.lock().map_err(|e| e.to_string())?;
    watchers.remove(&project_path);
    Ok(())
}

// ---- Session persistence ----

#[tauri::command]
pub async fn save_session(data: String, app: tauri::AppHandle) -> Result<(), String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&app_dir)
            .map_err(|e| format!("Failed to create app data dir: {}", e))?;
        let session_file = app_dir.join("session.json");
        std::fs::write(session_file, data).map_err(|e| format!("Failed to save session: {}", e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub fn load_session(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let session_file = app_dir.join("session.json");
    if session_file.exists() {
        let data = std::fs::read_to_string(session_file)
            .map_err(|e| format!("Failed to read session: {}", e))?;
        Ok(Some(data))
    } else {
        Ok(None)
    }
}

// ---- Settings (preferences) ----
// 偏好设置持久化到 app_data_dir/settings.json,与 session.json 同目录、同套路。
// 前端拿到原始 JSON 字符串自己解析/合并默认值,后端只负责存取。

#[tauri::command]
pub async fn save_settings(data: String, app: tauri::AppHandle) -> Result<(), String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&app_dir)
            .map_err(|e| format!("Failed to create app data dir: {}", e))?;
        let settings_file = app_dir.join("settings.json");
        std::fs::write(settings_file, data).map_err(|e| format!("Failed to save settings: {}", e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub fn load_settings(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let settings_file = app_dir.join("settings.json");
    if settings_file.exists() {
        let data = std::fs::read_to_string(settings_file)
            .map_err(|e| format!("Failed to read settings: {}", e))?;
        Ok(Some(data))
    } else {
        Ok(None)
    }
}

// ---- Maven ----

#[derive(Serialize)]
pub struct MavenModule {
    name: String,
    group_id: String,
    artifact_id: String,
    version: String,
    packaging: String,
    pom_path: String,
    modules: Vec<String>,
}

#[tauri::command]
pub async fn parse_maven_modules(project_path: String) -> Result<Vec<MavenModule>, String> {
    on_worker(move || {
        let mut modules = Vec::new();

        for entry in WalkDir::new(&project_path)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == "pom.xml" {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    if let Some(module) =
                        parse_pom(&content, entry.path().to_string_lossy().to_string())
                    {
                        modules.push(module);
                    }
                }
            }
        }

        Ok(modules)
    })
    .await
}

fn local_tag_name(raw: &[u8]) -> String {
    let full = String::from_utf8_lossy(raw).to_string();
    full.rsplit_once(':')
        .map_or(full.clone(), |(_, local)| local.to_string())
}

fn parse_pom(content: &str, pom_path: String) -> Option<MavenModule> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(content);
    let mut buf = Vec::new();
    let mut current_tag = String::new();
    let mut group_id = String::new();
    let mut artifact_id = String::new();
    let mut version = String::new();
    let mut packaging = String::from("jar");
    let mut child_modules = Vec::new();
    let mut depth = 0;
    let mut in_parent = false;
    let mut in_modules = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let tag = local_tag_name(e.name().as_ref());
                if tag == "parent" {
                    in_parent = true;
                }
                if tag == "modules" {
                    in_modules = true;
                }
                current_tag = tag;
            }
            Ok(Event::End(e)) => {
                let tag = local_tag_name(e.name().as_ref());
                if tag == "parent" {
                    in_parent = false;
                }
                if tag == "modules" {
                    in_modules = false;
                }
                depth -= 1;
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().trim().to_string();
                if !in_parent && depth == 2 {
                    match current_tag.as_str() {
                        "groupId" => group_id = text.clone(),
                        "artifactId" => artifact_id = text.clone(),
                        "version" => version = text.clone(),
                        "packaging" => packaging = text.clone(),
                        _ => {}
                    }
                }
                if in_modules && current_tag == "module" && !text.is_empty() {
                    child_modules.push(text);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    if artifact_id.is_empty() {
        return None;
    }

    Some(MavenModule {
        name: artifact_id.clone(),
        group_id,
        artifact_id,
        version,
        packaging,
        pom_path,
        modules: child_modules,
    })
}

#[tauri::command]
pub fn run_maven_command(
    project_path: String,
    goals: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut child = Command::new("mvn")
        .args(&goals)
        .current_dir(&project_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run mvn: {}", e))?;

    let stdout = child.stdout.take().ok_or("Failed to capture mvn stdout")?;
    let stderr = child.stderr.take().ok_or("Failed to capture mvn stderr")?;

    let app_out = app.clone();
    let app_err = app.clone();

    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines().flatten() {
            let _ = app_out.emit("maven-output", &line);
        }
    });

    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().flatten() {
            let _ = app_err.emit("maven-output", &line);
        }
    });

    std::thread::spawn(move || {
        let status = child.wait();
        let code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        let _ = app.emit("maven-done", code);
    });

    Ok(())
}

#[derive(Serialize, Clone)]
pub struct MavenCollectResult {
    pub exit_code: i32,
    pub output: String,
}

#[tauri::command]
pub async fn run_maven_collect(
    project_path: String,
    goals: Vec<String>,
) -> Result<MavenCollectResult, String> {
    tokio::task::spawn_blocking(move || {
        let result = Command::new("mvn")
            .args(&goals)
            .current_dir(&project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("Failed to run mvn: {}", e))?;

        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        let output = if stderr.is_empty() { stdout } else { format!("{}\n{}", stdout, stderr) };

        Ok(MavenCollectResult {
            exit_code: result.status.code().unwrap_or(-1),
            output,
        })
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}
