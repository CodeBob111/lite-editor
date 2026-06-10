// 文件系统监听(自 commands.rs 迁入)。事件经 EventSink 推送(原 Tauri emit)。

use crate::events::{CoreEvent, EventSink};
use crate::fs::should_skip;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct WatcherState {
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
}

pub fn start_file_watcher(
    project_path: String,
    events: Arc<dyn EventSink>,
    state: &WatcherState,
) -> Result<(), String> {
    let mut watchers = state.watchers.lock().map_err(|e| e.to_string())?;

    if watchers.contains_key(&project_path) {
        return Ok(());
    }

    let path_clone = project_path.clone();
    let last_emit_ms = Arc::new(AtomicU64::new(0));
    let last_clone = last_emit_ms.clone();
    let structural_flag = Arc::new(AtomicBool::new(false));
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
                events.emit(CoreEvent::FileChanged {
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

pub fn stop_file_watcher(project_path: &str, state: &WatcherState) -> Result<(), String> {
    let mut watchers = state.watchers.lock().map_err(|e| e.to_string())?;
    watchers.remove(project_path);
    Ok(())
}
