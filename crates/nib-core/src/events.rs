// core→宿主 的事件通道(替代 Tauri emit;RFC v2 §3)。
// 低频事件走本 trait 直推;高频源(终端输出等)不许走这里——按 dirty-notify+帧内拉取
// 设计(critic V4),终端模块 M4 才迁移。
// 宿主职责:src-tauri 侧映射回与旧前端完全一致的事件名/载荷;nib-app 侧派发进 gpui 主线程。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub enum CoreEvent {
    FileChanged {
        project: String,
        has_structural: bool,
        /// 本次(已防抖合并的)突发里实际变更的文件绝对路径,供宿主只刷受影响的标签,
        /// 不必把所有已打开文件全读一遍。
        paths: Vec<String>,
    },
    /// 原样转发 LSP textDocument/publishDiagnostics 的 params
    LspDiagnostics(serde_json::Value),
    LspProgress {
        language: String,
        kind: String,
        message: String,
        percentage: Option<u64>,
    },
    MavenOutput(String),
    MavenDone(i32),
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: CoreEvent);
}

/// 测试/无宿主场景用:丢弃所有事件
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: CoreEvent) {}
}
