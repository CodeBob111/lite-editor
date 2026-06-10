// nib-core:Nib 的 UI 无关后端内核。两个宿主共用:
// - src-tauri(过渡期旧 Nib):命令薄壳 + TauriSink,行为与历史完全一致
// - nib-app(gpui 原生版):事件派发进主线程,数据目录独立
// 约定:阻塞工作经 rt::on_worker/spawn_blocking 跑在 core 自持 runtime;
// 低频事件走 EventSink;高频源(终端)不经 EventSink(dirty-pull,M4)。

pub mod arthas;
pub mod astore;
pub mod clipboard;
pub mod diff;
pub mod events;
pub mod fs;
pub mod git;
pub mod java_index;
pub mod lsp;
pub mod markdown;
pub mod maven;
pub mod paths;
pub mod rt;
pub mod search;
pub mod terminal;
pub mod session;
pub mod usage_index;
pub mod watch;

use std::sync::Arc;

pub use events::{CoreEvent, EventSink, NullSink};
pub use paths::DataDirs;

pub struct NibCore {
    pub events: Arc<dyn EventSink>,
    pub dirs: DataDirs,
    pub lsp: lsp::LspState,
    pub watcher: watch::WatcherState,
    pub java_index: java_index::JavaIndexState,
    pub usage_index: usage_index::UsageIndexState,
    pub astore: astore::AstoreState,
}

impl NibCore {
    pub fn new(events: Arc<dyn EventSink>, dirs: DataDirs) -> Self {
        let astore = astore::AstoreState::new(dirs.astore_session.clone());
        Self {
            events,
            dirs,
            lsp: lsp::LspState::default(),
            watcher: watch::WatcherState::default(),
            java_index: java_index::JavaIndexState::default(),
            usage_index: usage_index::UsageIndexState::default(),
            astore,
        }
    }

    /// 应用退出前必须调用:杀干净 LSP 子进程。jdtls 不随父进程退出(实证:父进程
    /// 被杀后 ppid=1 仍存活,每个 ~1.5G 堆),孤儿化事故本项目记录在案——两个宿主
    /// 都要在各自的退出钩子里接这条(src-tauri: RunEvent::Exit;nib-app: gpui quit)。
    pub fn shutdown(&self) {
        self.lsp.kill_all();
    }
}
