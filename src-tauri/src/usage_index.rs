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
use std::time::UNIX_EPOCH;
use tauri::State;
use walkdir::WalkDir;

#[derive(Default)]
pub struct UsageIndexState {
    indices: Mutex<HashMap<String, UsageIndex>>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct FileTokens {
    modified: u64,
    // token_id(指向 UsageIndex.tokens 的下标)-> 升序去重的 1-based 行号
    tokens: HashMap<u32, Vec<u32>>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct UsageIndex {
    version: u32,
    // token 文本全局只存一份,id = 下标。各文件的出现表以 id 为键:同一个标识符
    // (如常用类名/方法名)出现在几千个文件里时,不再在每个文件重复存一份字符串,
    // 大仓索引内存可降 2-3 倍。表只增不删(词汇表基本稳定,全量重建时从零开始)。
    tokens: Vec<String>,
    files: HashMap<String, FileTokens>, // 文件路径 -> 该文件的 token 出现表
    // tokens 的反查表,构建时维护;落盘冗余,load 后由 inherit_tokens 重建。
    #[serde(skip)]
    token_ids: HashMap<String, u32>,
}

impl UsageIndex {
    // 新建一个继承 old 全局 token 表的空索引(增量构建沿用旧表,旧 FileTokens 的
    // id 在新索引里依然有效),并重建反查表。
    fn inherit_tokens(old: Option<&UsageIndex>) -> UsageIndex {
        let mut index = UsageIndex {
            version: INDEX_VERSION,
            tokens: old.map(|o| o.tokens.clone()).unwrap_or_default(),
            ..Default::default()
        };
        index.token_ids = index
            .tokens
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();
        index
    }

    fn intern(&mut self, tok: String) -> u32 {
        if let Some(&id) = self.token_ids.get(&tok) {
            return id;
        }
        let id = self.tokens.len() as u32;
        self.tokens.push(tok.clone());
        self.token_ids.insert(tok, id);
        id
    }

    fn insert_file(&mut self, path: String, modified: u64, raw: HashMap<String, Vec<u32>>) {
        let tokens = raw
            .into_iter()
            .map(|(tok, lines)| (self.intern(tok), lines))
            .collect();
        self.files.insert(path, FileTokens { modified, tokens });
    }
}

#[derive(Serialize, Clone)]
pub struct Usage {
    pub file: String,
    pub line: u32, // 1-based
    pub text: String,
}

// v2:FileTokens 的键从 token 文本改为全局 token id(见 UsageIndex.tokens)。
// 旧版磁盘缓存反序列化失败或版本不符都会被丢弃,触发一次全量重建。
const INDEX_VERSION: u32 = 2;

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
        .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|ext| ext == "java"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

// 增量构建:沿用旧索引里 mtime 没变的文件,只重扫变化/新增的。
// 并行阶段只做读文件+切词(无共享状态);intern 进全局 token 表是串行收尾(纯哈希,极快)。
fn build(files: &[PathBuf], old: Option<&UsageIndex>) -> UsageIndex {
    enum Scanned {
        Reused(String, FileTokens),
        Fresh(String, u64, HashMap<String, Vec<u32>>),
    }

    let scanned: Vec<Scanned> = files
        .par_iter()
        .filter_map(|path| {
            let modified = file_modified_secs(path);
            let path_str = path.to_string_lossy().to_string();
            if let Some(o) = old {
                if let Some(ft) = o.files.get(&path_str) {
                    if ft.modified == modified {
                        // id 指向旧 token 表;新索引继承旧表,克隆即可复用
                        return Some(Scanned::Reused(path_str, ft.clone()));
                    }
                }
            }
            let content = std::fs::read_to_string(path).ok()?;
            Some(Scanned::Fresh(path_str, modified, tokenize(&content)))
        })
        .collect();

    let mut index = UsageIndex::inherit_tokens(old);
    for item in scanned {
        match item {
            Scanned::Reused(path, ft) => {
                index.files.insert(path, ft);
            }
            Scanned::Fresh(path, modified, raw) => index.insert_file(path, modified, raw),
        }
    }
    index
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
pub async fn query_usages(
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
        let tid = match index.token_ids.get(&symbol) {
            Some(&id) => id,
            None => return Ok(Vec::new()),
        };
        for (path, ft) in &index.files {
            if let Some(lines) = ft.tokens.get(&tid) {
                for &ln in lines {
                    hits.push((path.clone(), ln));
                }
            }
        }
    }
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    // 命中行的文本要读一批文件:同步命令跑在主线程会卡 UI,搬到阻塞线程池。
    tokio::task::spawn_blocking(move || {
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
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

// 文件保存时:只重扫该文件、更新内存(不写盘——大索引逐次写盘太慢;
// 下次构建/启动的 mtime 增量会把磁盘补齐)。
#[tauri::command]
pub async fn update_usage_index_file(
    project_path: String,
    file_path: String,
    state: State<'_, UsageIndexState>,
) -> Result<(), String> {
    let fp = file_path.clone();
    let scanned = tokio::task::spawn_blocking(move || {
        let path = Path::new(&fp);
        if !path.exists() || path.extension().is_none_or(|e| e != "java") {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok::<_, String>(Some((file_modified_secs(path), tokenize(&content))))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))??;

    if let Some((modified, raw)) = scanned {
        let mut indices = state.indices.lock().map_err(|e| e.to_string())?;
        // 只在该项目索引已加载时更新;没加载就算了(下次构建会带上)。
        if let Some(index) = indices.get_mut(&project_path) {
            index.insert_file(file_path, modified, raw);
        }
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

    // 查某文件里某符号的行号(测试辅助:文本 → id → 行号表)。
    fn lines_of(idx: &UsageIndex, file: &str, tok: &str) -> Option<Vec<u32>> {
        let tid = idx.token_ids.get(tok)?;
        idx.files.get(file)?.tokens.get(tid).cloned()
    }

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
        assert_eq!(lines_of(&idx, &f1s, "helperMethod"), Some(vec![2]));
        assert_eq!(lines_of(&idx, &f2s, "helperMethod"), Some(vec![3]));
        // 全局驻留:同一 token 在两个文件里共用一个 id,文本只存一份
        assert_eq!(idx.tokens.iter().filter(|t| *t == "helperMethod").count(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn incremental_build_reuses_old_tokens_and_interns_new() {
        let dir = std::env::temp_dir().join(format!("usage_idx_incr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("A.java");
        std::fs::write(&f1, "class A {\n  void m() { helperMethod(); }\n}").unwrap();
        let idx1 = build(std::slice::from_ref(&f1), None);

        // 新增 B 后增量重建:A mtime 未变走 Reused(id 指旧表),B 走 Fresh intern
        let f2 = dir.join("B.java");
        std::fs::write(&f2, "class B {\n  void n() {\n    helperMethod();\n    brandNewSymbol();\n  }\n}").unwrap();
        let idx2 = build(&[f1.clone(), f2.clone()], Some(&idx1));

        let f1s = f1.to_string_lossy().to_string();
        let f2s = f2.to_string_lossy().to_string();
        assert_eq!(lines_of(&idx2, &f1s, "helperMethod"), Some(vec![2]), "复用的旧表 id 仍解析正确");
        assert_eq!(lines_of(&idx2, &f2s, "helperMethod"), Some(vec![3]));
        assert_eq!(lines_of(&idx2, &f2s, "brandNewSymbol"), Some(vec![4]), "新符号正常 intern");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn token_table_stays_small_across_many_files() {
        // 内存有界性的可断言代理:300 个文件共享同一批符号时,全局 token 表
        // 只存一份文本(≈ 唯一符号数),而不是 300 × 每文件符号数。
        let dir = std::env::temp_dir().join(format!("usage_idx_scale_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body: String = (0..40).map(|i| format!("  sharedSymbol{}();\n", i)).collect();
        let mut files = Vec::new();
        for i in 0..300 {
            let f = dir.join(format!("C{}.java", i));
            std::fs::write(&f, format!("class C{} {{\n{}}}\n", i, body)).unwrap();
            files.push(f);
        }

        let idx = build(&files, None);
        assert_eq!(idx.files.len(), 300);
        // 词汇表 = 40 个共享符号 + 300 个类名,远小于「按文件重复存」的 300×41
        assert!(
            idx.tokens.len() <= 40 + 300 + 10,
            "token 表未去重: {} 项",
            idx.tokens.len()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn serde_roundtrip_works_as_incremental_base() {
        let dir = std::env::temp_dir().join(format!("usage_idx_serde_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("A.java");
        std::fs::write(&f1, "class A {\n  void m() { helperMethod(); }\n}").unwrap();
        let idx = build(std::slice::from_ref(&f1), None);

        // 落盘 → 回读(token_ids 是 #[serde(skip)],回读后为空)→ 作为增量基底重建
        let json = serde_json::to_string(&idx).unwrap();
        let loaded: UsageIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.version, INDEX_VERSION);
        let rebuilt = build(std::slice::from_ref(&f1), Some(&loaded));

        let f1s = f1.to_string_lossy().to_string();
        assert_eq!(lines_of(&rebuilt, &f1s, "helperMethod"), Some(vec![2]));

        std::fs::remove_dir_all(&dir).ok();
    }
}
