// 全局搜索(ignore crate 并行遍历,边走边搜)与全量文件列举(自 commands.rs 迁入,逻辑不变)

use crate::fs::{is_binary_ext, should_skip};
use ignore::{WalkBuilder, WalkState};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use walkdir::WalkDir;

#[derive(Serialize, Clone)]
pub struct SearchResult {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub text: String,
}

pub async fn search_in_files(
    project_path: String,
    query: String,
    case_sensitive: Option<bool>,
    max_results: Option<usize>,
) -> Result<Vec<SearchResult>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    crate::rt::on_worker(move || {
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
}

pub async fn list_all_files(project_path: String) -> Result<Vec<String>, String> {
    crate::rt::on_worker(move || {
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
        std::fs::write(root.join("node_modules/pkg/b.ts"), "foo foo foo\n").unwrap();
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
}
