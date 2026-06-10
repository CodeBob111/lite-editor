use ignore::{WalkBuilder, WalkState};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;
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

/// 大小写不敏感字典序(近似前端 localeCompare 的 ASCII 行为),原串 tie-break
pub(crate) fn ci_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
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

// ---- Search (ignore crate 并行遍历,边走边搜) ----

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

        // 交互式搜索跳过超大文件(日志/生成物):整读进内存逐行扫只拖慢出结果。
        const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;

        let found_count = AtomicU64::new(0);
        let collected: Mutex<Vec<SearchResult>> = Mutex::new(Vec::new());

        // ignore::WalkBuilder(ripgrep 同款 walker):目录遍历本身并行,且边走边读边搜,
        // 不再「先串行收集全量文件列表、再并行搜」;同时尊重 .gitignore——
        // 被忽略的生成物/日志/vendor 连 stat 都不会发生,这是 should_skip 名单兜不全的部分。
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(12);
        WalkBuilder::new(&project_path)
            .hidden(false) // 隐藏文件照常搜(.env/.github);.git 等由 should_skip 兜
            .filter_entry(|e| !should_skip(&e.file_name().to_string_lossy()))
            .threads(threads)
            .build_parallel()
            .run(|| {
                let query_cmp = &query_cmp;
                let found_count = &found_count;
                let collected = &collected;
                Box::new(move |entry| {
                    if found_count.load(Ordering::Relaxed) >= max as u64 {
                        return WalkState::Quit;
                    }
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => return WalkState::Continue,
                    };
                    if !entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                        || is_binary_ext(&entry.file_name().to_string_lossy())
                        || entry.metadata().map(|m| m.len() > MAX_FILE_SIZE).unwrap_or(true)
                    {
                        return WalkState::Continue;
                    }

                    let content = match std::fs::read_to_string(entry.path()) {
                        Ok(c) => c,
                        Err(_) => return WalkState::Continue,
                    };

                    // 不区分大小写时整文件只做一次小写转换(每行 to_lowercase = 每行一次堆分配);
                    // to_lowercase 不增删换行,行序与原文 zip 对齐。
                    let haystack_owned;
                    let haystack: &str = if case_sensitive {
                        &content
                    } else {
                        haystack_owned = content.to_lowercase();
                        &haystack_owned
                    };
                    // 整文件预筛:绝大多数文件不含命中词,一次 contains 即可跳过逐行扫描。
                    if !haystack.contains(query_cmp) {
                        return WalkState::Continue;
                    }

                    let mut file_results = Vec::new();
                    for (i, (line_text, hay_line)) in content.lines().zip(haystack.lines()).enumerate() {
                        if found_count.load(Ordering::Relaxed) >= max as u64 {
                            break;
                        }
                        if let Some(col) = hay_line.find(query_cmp) {
                            found_count.fetch_add(1, Ordering::Relaxed);
                            file_results.push(SearchResult {
                                path: entry.path().to_string_lossy().to_string(),
                                line: i as u32,
                                column: col as u32,
                                text: line_text.trim().to_string(),
                            });
                        }
                    }
                    if !file_results.is_empty() {
                        collected.lock().unwrap().extend(file_results);
                    }
                    WalkState::Continue
                })
            });

        let mut results = collected.into_inner().unwrap();
        // 并行遍历的返回序不稳定:按 路径+行号 排序,同一查询两次结果顺序一致。
        results.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
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
pub async fn load_session(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    on_worker(move || {
        let session_file = app_dir.join("session.json");
        if session_file.exists() {
            let data = std::fs::read_to_string(session_file)
                .map_err(|e| format!("Failed to read session: {}", e))?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    })
    .await
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
pub async fn load_settings(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    on_worker(move || {
        let settings_file = app_dir.join("settings.json");
        if settings_file.exists() {
            let data = std::fs::read_to_string(settings_file)
                .map_err(|e| format!("Failed to read settings: {}", e))?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    })
    .await
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
        for line in reader.lines().map_while(Result::ok) {
            let _ = app_out.emit("maven-output", &line);
        }
    });

    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
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

// ---- Markdown 渲染(md 预览用) ----
// 自前端 marked 迁入(comrak,GFM 扩展对齐 marked gfm:true;不开硬换行 = breaks:false)。

fn markdown_to_html(text: &str) -> String {
    let mut options = comrak::Options::default();
    options.extension.autolink = true;
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    // 沿用 marked 的原始 HTML 透传现状(本地文件预览,信任模型不变)
    options.render.r#unsafe = true;
    comrak::markdown_to_html(text, &options)
}

#[tauri::command]
pub async fn render_markdown(text: String) -> Result<String, String> {
    on_worker(move || Ok(markdown_to_html(&text))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // 搭一个最小项目树:普通命中文件、node_modules 干扰项、超过 2MB 的大文件。
    fn setup_project(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("nib-search-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("src/a.ts"), "const Foo = 1;\nlet bar = foo();\n").unwrap();
        std::fs::write(root.join("node_modules/pkg/b.ts"), "foo in dependency\n").unwrap();
        std::fs::write(root.join("big.log"), format!("foo\n{}", "x".repeat(3 * 1024 * 1024))).unwrap();
        root
    }

    #[tokio::test]
    async fn search_case_insensitive_hits_and_skips() {
        let root = setup_project("ci");
        let results = search_in_files(root.to_string_lossy().to_string(), "foo".into(), Some(false), None)
            .await
            .unwrap();
        // 不区分大小写:src/a.ts 的 Foo(行0)和 foo()(行1)都命中;
        // node_modules 被 should_skip 跳过;big.log 超 2MB 上限被跳过。
        let paths: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.iter().all(|p| p.ends_with("src/a.ts")), "unexpected paths: {:?}", paths);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].line, 0);
        assert_eq!(results[1].line, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn search_case_sensitive_filters() {
        let root = setup_project("cs");
        let results = search_in_files(root.to_string_lossy().to_string(), "Foo".into(), Some(true), None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line, 0);
        assert_eq!(results[0].column, 6); // "const Foo" 中 Foo 的字节偏移
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn markdown_render_matches_marked_gfm_behavior() {
        // fenced code:语言 class + HTML 转义(对齐原 marked 自定义 renderer)
        let html = markdown_to_html("```rust\nlet a = 1 < 2;\n```\n");
        assert!(html.contains("<pre><code class=\"language-rust\">"), "html: {}", html);
        assert!(html.contains("1 &lt; 2"));
        // GFM autolink:裸 URL 可点
        let html = markdown_to_html("see https://example.com/x\n");
        assert!(html.contains("<a href=\"https://example.com/x\""), "html: {}", html);
        // GFM 表格
        let html = markdown_to_html("| a | b |\n| - | - |\n| 1 | 2 |\n");
        assert!(html.contains("<table>"));
        // 原始 HTML 透传(marked 默认不消毒,行为保持)
        let html = markdown_to_html("<div class=\"x\">hi</div>\n");
        assert!(html.contains("<div class=\"x\">hi</div>"));
        // 软换行不转 <br>(对齐 breaks:false)
        let html = markdown_to_html("line1\nline2\n");
        assert!(!html.contains("<br"));
    }
}
