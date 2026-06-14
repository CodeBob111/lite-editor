// 文件系统监听(自 commands.rs 迁入)。事件经 EventSink 推送(原 Tauri emit)。

use crate::events::{CoreEvent, EventSink};
use crate::fs::should_skip;
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 尾沿防抖静默窗口:突发事件停止 DEBOUNCE 后才合并发一次 FileChanged。
/// 前沿节流(旧实现)会在突发首个事件就发(载荷不全),且突发末尾设的结构变更标志若落在
/// 窗口内可能永不发出(漏事件)。尾沿防抖累积整段突发再发一次,既不漏也不风暴。
const DEBOUNCE: Duration = Duration::from_millis(300);

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
    // notify 回调(watcher 线程,每事件一次)只做廉价过滤后把 (路径, 是否结构变更) 投进 channel;
    // 真正的累积/防抖/emit 交给下面独立的防抖线程,绝不在回调里阻塞或 emit。
    let (tx, rx) = mpsc::channel::<(Vec<String>, bool)>();

    // 防抖线程:收第一个事件后进入收集窗口,持续 recv_timeout(DEBOUNCE);静默够久 → 合并
    // emit 一次(带累积的变更路径集 + 结构变更标志)。tx 随 watcher 一起 drop → rx 断开 → 退出。
    {
        let events = events.clone();
        let project = project_path.clone();
        std::thread::spawn(move || loop {
            let mut paths: HashSet<String> = HashSet::new();
            let mut structural = false;
            // 阻塞等突发的第一个事件
            match rx.recv() {
                Ok((ps, s)) => {
                    paths.extend(ps);
                    structural |= s;
                }
                Err(_) => return, // watcher 已停
            }
            // 收集窗口:静默 DEBOUNCE 后退出
            let disconnected = loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok((ps, s)) => {
                        paths.extend(ps);
                        structural |= s;
                    }
                    Err(RecvTimeoutError::Timeout) => break false,
                    Err(RecvTimeoutError::Disconnected) => break true,
                }
            };
            events.emit(CoreEvent::FileChanged {
                project: project.clone(),
                has_structural: structural,
                paths: paths.into_iter().collect(),
            });
            if disconnected {
                return;
            }
        });
    }

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // 忽略构建产物/依赖/VCS 目录(target、.git、node_modules、.metadata 等)的事件:
            // 新项目打开时 jdtls 会触发 Maven 构建,往 target/ 写大量 .class。否则这些事件会把
            // 树刷新(readDirTree + 展开态全树遍历)刷爆,索引期间界面持续卡顿。
            // 按项目根的相对路径判断,避免祖先目录恰好叫 out/bin 之类时全部误伤。
            if event.paths.iter().all(|p| {
                p.strip_prefix(&path_clone)
                    .map(|rel| {
                        rel.components()
                            .any(|c| c.as_os_str().to_str().map(should_skip).unwrap_or(false))
                    })
                    .unwrap_or(false)
            }) {
                return;
            }
            // 结构变化 = 影响文件树拓扑的事件:创建、删除,以及**重命名**。macOS(FSEvents)
            // 的重命名既不是 Create 也不是 Remove,而是 Modify(Name(...)) —— 漏掉它,外部
            // 重命名(Finder / git / 另一编辑器)就不会触发 reload_tree,树显示旧名。
            let structural = matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
            );
            if !structural && !matches!(event.kind, EventKind::Modify(_)) {
                return; // 只关心 创建/删除/修改
            }
            let ps: Vec<String> = event
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let _ = tx.send((ps, structural));
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
