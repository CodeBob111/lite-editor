// 持久化「符号 → 出现位置」倒排索引:为 find-usages 提供不依赖 jdtls 的瞬时结果。
//
// 设计:按文件存 token→行号(增量友好,保存时只重扫单个文件);查询时遍历各文件做一次
// HashMap 查找(几千文件 = 几千次查找,亚毫秒级),命中后按文件分组、各读一次取行文本。
// 内存即权威:保存时只更新内存(大索引逐次写盘太慢);磁盘缓存在「构建」时写一次,
// 下次启动按 mtime 增量重扫被改过的文件即可补齐——崩溃也不丢正确性。

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;
use walkdir::WalkDir;

#[derive(Default)]
pub struct UsageIndexState {
    indices: Mutex<HashMap<String, UsageIndex>>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct FileTokens {
    modified: u64,
    // token -> 升序去重的 1-based 行号
    tokens: HashMap<String, Vec<u32>>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct UsageIndex {
    version: u32,
    files: HashMap<String, FileTokens>, // 文件路径 -> 该文件的 token 出现表
}

#[derive(Serialize, Clone)]
pub struct Usage {
    pub file: String,
    pub line: u32, // 1-based
    pub text: String,
}

const INDEX_VERSION: u32 = 1;

fn cache_path(project_path: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let hash = project_path.replace('/', "_");
    PathBuf::from(format!(
        "{}/Library/Caches/lite-editor/usage-index/{}.json",
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

// 跳过的关键字/基本类型:它们不是 find-usages 的目标,不索引可显著减小体积。
fn is_noise(tok: &str) -> bool {
    matches!(
        tok,
        "public" | "private" | "protected" | "static" | "final" | "void" | "int" | "long"
            | "boolean" | "double" | "float" | "char" | "byte" | "short" | "class" | "interface"
            | "enum" | "extends" | "implements" | "return" | "new" | "this" | "super" | "import"
            | "package" | "throws" | "throw" | "try" | "catch" | "finally" | "for" | "while"
            | "switch" | "case" | "break" | "continue" | "instanceof" | "null" | "true" | "false"
            | "abstract" | "synchronized" | "volatile" | "transient" | "native" | "default"
            | "String" | "List" | "Map" | "Set" | "Object" | "Integer" | "Long" | "Boolean"
    )
}

// 把一份源码切成 token→行号表。只收长度 ≥3 的标识符,跳过噪声词。
fn tokenize(content: &str) -> HashMap<String, Vec<u32>> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for (i, line) in content.lines().enumerate() {
        let lineno = (i + 1) as u32;
        let bytes = line.as_bytes();
        let mut j = 0;
        while j < bytes.len() {
            let c = bytes[j];
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = j;
                j += 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let tok = &line[start..j];
                if tok.len() >= 3 && !is_noise(tok) {
                    let v = map.entry(tok.to_string()).or_default();
                    if v.last() != Some(&lineno) {
                        v.push(lineno);
                    }
                }
            } else {
                j += 1;
            }
        }
    }
    map
}

fn collect_java_files(project_path: &str) -> Vec<PathBuf> {
    let skip_dirs = [
        "target", "build", ".git", ".idea", "node_modules", ".settings", "bin", ".metadata",
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

// 增量构建:沿用旧索引里 mtime 没变的文件,只重扫变化/新增的。
fn build(files: &[PathBuf], old: Option<&UsageIndex>) -> UsageIndex {
    let entries: Vec<(String, FileTokens)> = files
        .par_iter()
        .filter_map(|path| {
            let modified = file_modified_secs(path);
            let path_str = path.to_string_lossy().to_string();
            if let Some(o) = old {
                if let Some(ft) = o.files.get(&path_str) {
                    if ft.modified == modified {
                        return Some((path_str, ft.clone()));
                    }
                }
            }
            let content = std::fs::read_to_string(path).ok()?;
            Some((
                path_str,
                FileTokens {
                    modified,
                    tokens: tokenize(&content),
                },
            ))
        })
        .collect();

    UsageIndex {
        version: INDEX_VERSION,
        files: entries.into_iter().collect(),
    }
}

#[tauri::command]
pub async fn build_usage_index(
    project_path: String,
    state: State<'_, UsageIndexState>,
) -> Result<u32, String> {
    let pp = project_path.clone();
    let index = tokio::task::spawn_blocking(move || {
        let cp = cache_path(&pp);
        let old = if cp.exists() {
            std::fs::read_to_string(&cp)
                .ok()
                .and_then(|s| serde_json::from_str::<UsageIndex>(&s).ok())
                .filter(|i| i.version == INDEX_VERSION)
        } else {
            None
        };
        let files = collect_java_files(&pp);
        let index = build(&files, old.as_ref());
        if let Some(parent) = cp.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&index) {
            let _ = std::fs::write(&cp, json);
        }
        index
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?;

    let count = index.files.len() as u32;
    let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
    indices.insert(project_path, index);
    Ok(count)
}

#[tauri::command]
pub fn query_usages(
    project_path: String,
    symbol: String,
    limit: usize,
    state: State<'_, UsageIndexState>,
) -> Result<Vec<Usage>, String> {
    // 锁内只收集命中 (file, line),随后释放锁再读文件文本,避免持锁做 I/O。
    let mut hits: Vec<(String, u32)> = Vec::new();
    {
        let indices = state.indices.lock().map_err(|e| e.to_string())?;
        let index = match indices.get(&project_path) {
            Some(i) => i,
            None => return Ok(Vec::new()),
        };
        for (path, ft) in &index.files {
            if let Some(lines) = ft.tokens.get(&symbol) {
                for &ln in lines {
                    hits.push((path.clone(), ln));
                }
            }
        }
    }
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    hits.sort();
    let cap = if limit == 0 { usize::MAX } else { limit };

    // 按文件分组,各读一次,取需要的行文本。
    let mut by_file: HashMap<String, Vec<u32>> = HashMap::new();
    for (path, ln) in hits.into_iter().take(cap) {
        by_file.entry(path).or_default().push(ln);
    }
    let mut out: Vec<Usage> = Vec::new();
    for (path, mut lines) in by_file {
        lines.sort();
        if let Ok(content) = std::fs::read_to_string(&path) {
            let src: Vec<&str> = content.lines().collect();
            for ln in lines {
                let text = src
                    .get((ln as usize).saturating_sub(1))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                out.push(Usage {
                    file: path.clone(),
                    line: ln,
                    text,
                });
            }
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    Ok(out)
}

// 文件保存时:只重扫该文件、更新内存(不写盘——大索引逐次写盘太慢;
// 下次构建/启动的 mtime 增量会把磁盘补齐)。
#[tauri::command]
pub fn update_usage_index_file(
    project_path: String,
    file_path: String,
    state: State<'_, UsageIndexState>,
) -> Result<(), String> {
    let path = Path::new(&file_path);
    if !path.exists() || path.extension().map_or(true, |e| e != "java") {
        return Ok(());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let ft = FileTokens {
        modified: file_modified_secs(path),
        tokens: tokenize(&content),
    };
    let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
    // 只在该项目索引已加载时更新;没加载就算了(下次构建会带上)。
    if let Some(index) = indices.get_mut(&project_path) {
        index.files.insert(file_path, ft);
    }
    Ok(())
}

#[tauri::command]
pub fn remove_usage_index_file(
    project_path: String,
    file_path: String,
    state: State<'_, UsageIndexState>,
) -> Result<(), String> {
    let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
    if let Some(index) = indices.get_mut(&project_path) {
        index.files.remove(&file_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_collects_lines_skips_noise_and_short() {
        // 行1: class 头;行2/3 各出现一次 getRecommendRateIds
        let src = "public class Foo {\n  void bar() { getRecommendRateIds(); }\n  Object o = getRecommendRateIds() + id;\n}";
        let m = tokenize(src);
        assert_eq!(m.get("getRecommendRateIds"), Some(&vec![2, 3]), "符号按行号去重收集");
        assert!(!m.contains_key("public"), "关键字跳过");
        assert!(!m.contains_key("class"), "关键字跳过");
        assert!(!m.contains_key("void"), "关键字跳过");
        assert!(!m.contains_key("id"), "长度<3 跳过");
        assert!(m.contains_key("Foo"), "类名(len 3)保留");
        assert_eq!(m.get("bar"), Some(&vec![2]));
    }

    #[test]
    fn build_finds_symbol_across_files_with_lines() {
        let dir = std::env::temp_dir().join(format!("usage_idx_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("A.java");
        std::fs::write(&f1, "class A {\n  void m() { helperMethod(); }\n}").unwrap();
        let f2 = dir.join("B.java");
        std::fs::write(&f2, "class B {\n  void n() {\n    helperMethod();\n  }\n}").unwrap();

        let idx = build(&[f1.clone(), f2.clone()], None);
        assert_eq!(idx.files.len(), 2);
        let f1s = f1.to_string_lossy().to_string();
        let f2s = f2.to_string_lossy().to_string();
        assert_eq!(idx.files[&f1s].tokens.get("helperMethod"), Some(&vec![2]));
        assert_eq!(idx.files[&f2s].tokens.get("helperMethod"), Some(&vec![3]));

        std::fs::remove_dir_all(&dir).ok();
    }




}
