// 引用/搜索浮层下方的「代码预览」共享数据:整份文件内容用 Arc<String> 共享 + 行起始
// 字节偏移,渲染时(虚拟列表里)按需切片取可见行,**不预先把全文拆成 Vec<String>**——
// 后者是每行一次堆分配的性能回归。读盘由调用方异步完成(不阻塞 UI 线程)。

use std::sync::Arc;

pub struct PreviewDoc {
    /// 文档绝对路径,用于「选中项还在同一文件 → 不重读」的缓存判定。
    pub path: String,
    pub content: Arc<String>,
    /// 每行起始字节偏移;第 i 行 = content[line_starts[i] .. line_starts[i+1]]。
    pub line_starts: Arc<Vec<usize>>,
    /// 命中行(0-based,已 clamp)。
    pub match_line: usize,
}

impl PreviewDoc {
    pub fn new(path: String, content: String, match_line: usize) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in content.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        let match_line = match_line.min(starts.len().saturating_sub(1));
        Self {
            path,
            content: Arc::new(content),
            line_starts: Arc::new(starts),
            match_line,
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}

/// 从「内容 + 行偏移」切出第 i 行文本(去行尾 \r\n)。给虚拟列表渲染闭包用,
/// 闭包捕获 content/line_starts 两个 Arc(克隆廉价),只对可见行做这步、不碰全文。
pub fn line_text(content: &str, line_starts: &[usize], i: usize) -> String {
    let start = line_starts[i];
    let end = line_starts.get(i + 1).copied().unwrap_or(content.len());
    content[start..end].trim_end_matches(['\n', '\r']).to_string()
}
