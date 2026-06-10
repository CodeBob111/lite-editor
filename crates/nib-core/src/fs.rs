// 文件树与文件 CRUD(自 src-tauri commands.rs 迁入,逻辑不变)

use crate::rt::on_worker;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize, Clone)]
pub struct FileNode {
    name: String,
    path: String,
    #[serde(rename = "isDir")]
    is_dir: bool,
    children: Option<Vec<FileNode>>,
}

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

pub(crate) fn should_skip(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".idea" | ".vscode" | "node_modules" | "target" | ".DS_Store" | "__pycache__"
            // 构建/产物目录:否则会搜到已删代码在编译/打包输出里的陈旧副本。
            | "build" | "dist" | "out" | "bin" | ".gradle" | ".next" | ".nuxt" | ".turbo"
            | "coverage" | ".settings" | ".metadata"
    )
}

pub(crate) fn is_binary_ext(name: &str) -> bool {
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

pub async fn read_file(path: String) -> Result<String, String> {
    on_worker(move || {
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path, e))
    })
    .await
}

pub async fn write_file(path: String, content: String) -> Result<(), String> {
    on_worker(move || {
        std::fs::write(&path, &content).map_err(|e| format!("Failed to write {}: {}", path, e))
    })
    .await
}

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

pub async fn create_dir(path: String) -> Result<(), String> {
    on_worker(move || {
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("Failed to create directory {}: {}", path, e))
    })
    .await
}

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

pub async fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    on_worker(move || {
        if Path::new(&new_path).exists() {
            return Err(format!("Target already exists: {}", new_path));
        }
        std::fs::rename(&old_path, &new_path).map_err(|e| format!("Failed to rename: {}", e))
    })
    .await
}
