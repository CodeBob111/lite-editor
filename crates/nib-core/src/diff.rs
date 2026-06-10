// 行级 diff 内核(M2c 自绘 diff 视图的数据层)。similar crate(Myers 算法)
// 产出双栏对齐的行对:左=旧(HEAD/暂存),右=新(工作区)。UI 只管渲染行对,
// 不在主线程算 diff——一律经 rt 在 core runtime 上跑。

use crate::rt::on_worker;
use serde::Serialize;
use similar::{ChangeTag, TextDiff};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    Same,
    Added,
    Removed,
}

/// 双栏对齐的一行:左右行号为 None 表示该侧此处是空槽(对齐占位)
#[derive(Serialize, Clone, Debug)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct DiffStat {
    pub added: usize,
    pub removed: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct LineDiff {
    pub rows: Vec<DiffRow>,
    pub stat: DiffStat,
}

pub(crate) fn diff_lines_sync(old: &str, new: &str) -> LineDiff {
    let diff = TextDiff::from_lines(old, new);
    let mut rows = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches('\n').to_string();
        match change.tag() {
            ChangeTag::Equal => rows.push(DiffRow {
                kind: DiffKind::Same,
                old_line: change.old_index().map(|i| i as u32 + 1),
                new_line: change.new_index().map(|i| i as u32 + 1),
                text,
            }),
            ChangeTag::Delete => {
                removed += 1;
                rows.push(DiffRow {
                    kind: DiffKind::Removed,
                    old_line: change.old_index().map(|i| i as u32 + 1),
                    new_line: None,
                    text,
                });
            }
            ChangeTag::Insert => {
                added += 1;
                rows.push(DiffRow {
                    kind: DiffKind::Added,
                    old_line: None,
                    new_line: change.new_index().map(|i| i as u32 + 1),
                    text,
                });
            }
        }
    }

    LineDiff {
        rows,
        stat: DiffStat { added, removed },
    }
}

/// 工作区文件 vs HEAD 的行级 diff(读盘+git show+diff 全在 core runtime)
pub async fn diff_file_against_head(cwd: String, rel_path: String) -> Result<LineDiff, String> {
    on_worker(move || {
        let head = crate::git::show_head_file_sync(&cwd, &rel_path).unwrap_or_default();
        let work = std::fs::read_to_string(std::path::Path::new(&cwd).join(&rel_path))
            .unwrap_or_default();
        Ok(diff_lines_sync(&head, &work))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_text_has_no_changes() {
        let d = diff_lines_sync("a\nb\n", "a\nb\n");
        assert_eq!(d.stat.added, 0);
        assert_eq!(d.stat.removed, 0);
        assert!(d.rows.iter().all(|r| r.kind == DiffKind::Same));
    }

    #[test]
    fn detects_add_remove_with_line_numbers() {
        let d = diff_lines_sync("a\nb\nc\n", "a\nx\nc\nd\n");
        assert_eq!(d.stat.removed, 1, "b 被删");
        assert_eq!(d.stat.added, 2, "x、d 新增");
        let removed: Vec<_> = d.rows.iter().filter(|r| r.kind == DiffKind::Removed).collect();
        assert_eq!(removed[0].text, "b");
        assert_eq!(removed[0].old_line, Some(2));
        assert_eq!(removed[0].new_line, None);
        let added: Vec<_> = d.rows.iter().filter(|r| r.kind == DiffKind::Added).collect();
        assert_eq!(added[0].text, "x");
        assert_eq!(added[0].new_line, Some(2));
        assert_eq!(added[1].text, "d");
        assert_eq!(added[1].new_line, Some(4));
    }

    #[test]
    fn new_file_is_all_added() {
        let d = diff_lines_sync("", "a\nb\n");
        assert_eq!(d.stat.added, 2);
        assert_eq!(d.stat.removed, 0);
    }
}
