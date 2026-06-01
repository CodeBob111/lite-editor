use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;
use walkdir::WalkDir;

#[derive(Default)]
pub struct JavaIndexState {
    indices: Mutex<HashMap<String, JavaIndex>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct IndexEntry {
    path: String,
    package: String,
    modified: u64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct JavaIndex {
    version: u32,
    built_at: u64,
    classes: HashMap<String, Vec<IndexEntry>>,
}

#[derive(Serialize, Clone)]
pub struct ClassLocation {
    pub path: String,
    pub package: String,
    pub fqn: String,
}

fn index_cache_path(project_path: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let hash = project_path.replace('/', "_");
    PathBuf::from(format!(
        "{}/Library/Caches/lite-editor/java-index/{}.json",
        home, hash
    ))
}

fn file_modified_secs(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse_package(content: &str) -> String {
    for line in content.lines().take(30) {
        let trimmed = line.trim();
        if trimmed.starts_with("package ") {
            if let Some(pkg) = trimmed
                .strip_prefix("package ")
                .and_then(|s| s.strip_suffix(';'))
            {
                return pkg.trim().to_string();
            }
        }
        if trimmed.starts_with("import ")
            || trimmed.starts_with("public ")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("@")
        {
            break;
        }
    }
    String::new()
}

fn collect_java_files(project_path: &str) -> Vec<PathBuf> {
    let skip_dirs = [
        "target",
        "build",
        ".git",
        ".idea",
        "node_modules",
        ".settings",
        "bin",
        ".metadata",
        ".classpath",
    ];

    WalkDir::new(project_path)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !skip_dirs.contains(&name.as_ref())
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| ext == "java"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn build_index_from_files(files: &[PathBuf], old_index: Option<&JavaIndex>) -> JavaIndex {
    let entries: Vec<(String, IndexEntry)> = files
        .par_iter()
        .filter_map(|path| {
            let class_name = path.file_stem()?.to_str()?.to_string();
            if class_name.starts_with('.') {
                return None;
            }

            let modified = file_modified_secs(path);
            let path_str = path.to_string_lossy().to_string();

            if let Some(old) = old_index {
                if let Some(entries) = old.classes.get(&class_name) {
                    if let Some(entry) = entries.iter().find(|e| e.path == path_str) {
                        if entry.modified == modified {
                            return Some((class_name, entry.clone()));
                        }
                    }
                }
            }

            let content = std::fs::read_to_string(path).ok()?;
            let package = parse_package(&content);

            Some((
                class_name,
                IndexEntry {
                    path: path_str,
                    package,
                    modified,
                },
            ))
        })
        .collect();

    let mut classes: HashMap<String, Vec<IndexEntry>> = HashMap::new();
    for (name, entry) in entries {
        classes.entry(name).or_default().push(entry);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    JavaIndex {
        version: 1,
        built_at: now,
        classes,
    }
}

#[tauri::command]
pub async fn build_java_index(
    project_path: String,
    state: State<'_, JavaIndexState>,
) -> Result<u32, String> {
    let pp = project_path.clone();
    let (index, class_count) = tokio::task::spawn_blocking(move || {
        let cache_path = index_cache_path(&pp);
        let old_index = if cache_path.exists() {
            std::fs::read_to_string(&cache_path)
                .ok()
                .and_then(|s| serde_json::from_str::<JavaIndex>(&s).ok())
        } else {
            None
        };
        let files = collect_java_files(&pp);
        let index = build_index_from_files(&files, old_index.as_ref());
        let class_count = index.classes.len() as u32;
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&index) {
            let _ = std::fs::write(&cache_path, json);
        }
        (index, class_count)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?;

    let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
    indices.insert(project_path, index);
    Ok(class_count)
}

#[tauri::command]
pub async fn load_java_index(
    project_path: String,
    state: State<'_, JavaIndexState>,
) -> Result<u32, String> {
    let pp = project_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let cache_path = index_cache_path(&pp);
        if !cache_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&cache_path)
            .map_err(|e| format!("Failed to read index: {}", e))?;
        let index: JavaIndex =
            serde_json::from_str(&content).map_err(|e| format!("Invalid index: {}", e))?;
        Ok::<_, String>(Some(index))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))??;

    match result {
        None => Ok(0),
        Some(index) => {
            let count = index.classes.len() as u32;
            let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
            indices.insert(project_path, index);
            Ok(count)
        }
    }
}

#[tauri::command]
pub fn search_java_class(
    project_path: String,
    class_name: String,
    state: State<'_, JavaIndexState>,
) -> Result<Vec<ClassLocation>, String> {
    let indices = state.indices.lock().map_err(|e| e.to_string())?;
    let index = match indices.get(&project_path) {
        Some(idx) => idx,
        None => return Ok(Vec::new()),
    };

    let entries = match index.classes.get(&class_name) {
        Some(e) => e,
        None => return Ok(Vec::new()),
    };

    Ok(entries
        .iter()
        .map(|e| ClassLocation {
            path: e.path.clone(),
            package: e.package.clone(),
            fqn: if e.package.is_empty() {
                class_name.clone()
            } else {
                format!("{}.{}", e.package, class_name)
            },
        })
        .collect())
}

#[tauri::command]
pub fn update_java_index_file(
    project_path: String,
    file_path: String,
    state: State<'_, JavaIndexState>,
) -> Result<(), String> {
    let path = Path::new(&file_path);
    if !path.exists() || path.extension().map_or(true, |e| e != "java") {
        return Ok(());
    }

    let class_name = path
        .file_stem()
        .and_then(|n| n.to_str())
        .ok_or("Invalid file name")?
        .to_string();

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let package = parse_package(&content);
    let modified = file_modified_secs(path);

    let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
    let index = indices.entry(project_path.clone()).or_default();

    let entries = index.classes.entry(class_name).or_default();
    if let Some(existing) = entries.iter_mut().find(|e| e.path == file_path) {
        existing.package = package;
        existing.modified = modified;
    } else {
        entries.push(IndexEntry {
            path: file_path,
            package,
            modified,
        });
    }

    let cache_path = index_cache_path(&project_path);
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&index) {
        let _ = std::fs::write(&cache_path, json);
    }

    Ok(())
}

#[tauri::command]
pub fn remove_java_index_file(
    project_path: String,
    file_path: String,
    state: State<'_, JavaIndexState>,
) -> Result<(), String> {
    let path = Path::new(&file_path);
    let class_name = match path.file_stem().and_then(|n| n.to_str()) {
        Some(n) => n.to_string(),
        None => return Ok(()),
    };

    let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
    let index = match indices.get_mut(&project_path) {
        Some(idx) => idx,
        None => return Ok(()),
    };

    if let Some(entries) = index.classes.get_mut(&class_name) {
        entries.retain(|e| e.path != file_path);
        if entries.is_empty() {
            index.classes.remove(&class_name);
        }
    }

    let cache_path = index_cache_path(&project_path);
    if let Ok(json) = serde_json::to_string(&index) {
        let _ = std::fs::write(&cache_path, json);
    }

    Ok(())
}
