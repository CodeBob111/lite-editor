//! 监控本机 Claude Code 会话的「回合结束」,零配置兜底地知道「CC 在某项目里跑完了一轮」。
//!
//! 机制:watch `~/.claude/projects/`(recursive,兼容目标会话目录尚未创建的首跑场景),把事件
//! 过滤到「项目 cwd 对应的会话目录」,对其下被追加的 `*.jsonl` tail 新增字节,遇到一条
//! `type=assistant` + 非 sidechain + `message.stop_reason=="end_turn"` 的记录即一个主回合结束 → 回调。
//!
//! 盲区(对比精确的 CC Stop hook):cwd 是「猜」的——把 Nib 终端的 project_root 编码成会话目录名。
//! 若 CC 不是在 project_root 启动(用户先 `cd` 进子目录、或在别处跑 claude),算出的目录不对,
//! 则静默不触发(不是降级,是完全收不到)。这正是 Stop hook 要补的缺口。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// 把项目 cwd 编码成 CC 的会话目录名:`/` `.` `_` 都替换成 `-`。
/// 例:`/Users/x/Open-Design.app` → `-Users-x-Open-Design-app`。
/// CC 把会话放在 `~/.claude/projects/<编码>/<session-id>.jsonl`。
pub fn encode_cc_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' || c == '_' { '-' } else { c })
        .collect()
}

/// 项目 cwd → 它的 CC 会话目录绝对路径(`~/.claude/projects/<编码>`)。
pub fn cc_project_dir(cwd: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(
        home.join(".claude")
            .join("projects")
            .join(encode_cc_dir_name(cwd)),
    )
}

/// 一行 jsonl 是否「一个主回合结束」:assistant 消息、非子 agent(sidechain)、stop_reason=end_turn,
/// 且含至少一个**非 thinking** 内容块。中途调工具的 assistant 是 `tool_use`、子 agent 是 sidechain,
/// 都不算。
///
/// 关键:CC 一个回合会写**两条** end_turn 行——先 thinking 块、再 text 块(同 msg_id,但可能差几十秒)。
/// 若两条都触发,5s 去重窗兜不住 → 一回合弹好几次。故只认含用户可见消息(text 等)的那条,跳过
/// thinking-only 行;它紧挨 Stop hook,两路信号才能被 ~5s 去重合并成一次。
pub fn line_is_turn_end(line: &str) -> bool {
    turn_end_msg_id(line).is_some()
}

/// 若该行是「主回合结束」,返回其 assistant 消息 id(无 id 字段则空串),否则 None。
/// 用来按 msg_id 去重:CC 一个回合可能写多条 end_turn 行(thinking + 多个 text 块),同一 msg_id,
/// 只该触发一次。
pub fn turn_end_msg_id(line: &str) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant")
        || v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true)
        || v.get("message")
            .and_then(|m| m.get("stop_reason"))
            .and_then(|s| s.as_str())
            != Some("end_turn")
    {
        return None;
    }
    // content 是数组时:必须有非 thinking 块(跳过 thinking-only 行,它比最终 text 早写);
    // 异常无数组 → 保守当回合结束
    let is_end = match v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(blocks) => blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) != Some("thinking")),
        None => true,
    };
    if !is_end {
        return None;
    }
    Some(
        v.get("message")
            .and_then(|m| m.get("id"))
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string(),
    )
}

/// CC 回合监控句柄:持有 notify watcher,drop 即停。
pub struct CcTurnWatcher {
    _watcher: RecommendedWatcher,
}

/// 为某项目 cwd 起一个 CC 回合监控。回合结束时(可能多次)调 `on_turn_done`(在 notify 线程上,
/// 必须只做非阻塞投递,如 channel send)。`~/.claude/projects` 不存在 → 返回 None(无 CC,不致命)。
pub fn watch_project_turns(
    cwd: &Path,
    on_turn_done: Arc<dyn Fn() + Send + Sync>,
) -> Option<CcTurnWatcher> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let projects_root = home.join(".claude").join("projects");
    if !projects_root.is_dir() {
        return None;
    }
    let target_dir = cc_project_dir(cwd)?;

    // 偏移表:每个 jsonl 文件「已消费到的字节」。预置已存在的目标目录文件到各自 EOF——
    // 只关心 watch 开始后的新增,绝不重解析 73MB 历史。
    let offsets: Arc<Mutex<HashMap<PathBuf, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    if let Ok(entries) = std::fs::read_dir(&target_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "jsonl") {
                if let Ok(meta) = e.metadata() {
                    offsets.lock().unwrap().insert(p, meta.len());
                }
            }
        }
    }

    // 上次已触发的 msg_id:一个回合可能写多条 end_turn 行(同 msg_id),只触发一次。
    let last_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cb_offsets = offsets.clone();
    let cb_target = target_dir.clone();
    let cb_last = last_id.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
            return;
        }
        for path in event.paths {
            // 只看目标项目目录下的 *.jsonl(recursive watch 会带来全机器其它会话的事件)
            if path.starts_with(&cb_target)
                && path.extension().is_some_and(|x| x == "jsonl")
                && tail_has_turn_end(&path, &cb_offsets, &cb_last)
            {
                on_turn_done();
            }
        }
    })
    .ok()?;
    // 监控 projects 根:目标会话目录可能在「项目首次跑 CC」时才被创建,直接 watch 目标目录会漏。
    watcher
        .watch(&projects_root, RecursiveMode::Recursive)
        .ok()?;
    Some(CcTurnWatcher { _watcher: watcher })
}

/// 监控 CC Stop hook 的事件目录(精确路):hook(gated on NIB_TERMINAL)每次写一个事件文件,
/// 新文件出现即一个「Nib 里的 CC 回合结束」→ 回调。文件随即删除(消费 + 防堆积)。启动时先清掉
/// 堆积的陈旧事件(Nib 没开时写的,补报无意义)。回调在 notify 线程,只做非阻塞投递。
pub fn watch_event_dir(
    dir: &Path,
    on_event: Arc<dyn Fn() + Send + Sync>,
) -> Option<CcTurnWatcher> {
    std::fs::create_dir_all(dir).ok()?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            return;
        }
        let mut fired = false;
        for path in event.paths {
            if path.extension().is_some_and(|x| x == "json") {
                let _ = std::fs::remove_file(&path); // 消费掉,防重复触发 + 防堆积
                fired = true;
            }
        }
        if fired {
            on_event();
        }
    })
    .ok()?;
    watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(CcTurnWatcher { _watcher: watcher })
}

/// tail 文件 `[已消费偏移, 当前EOF)` 的新增字节,推进偏移(只跨过完整行,半行留到下次),
/// 其中是否出现**新的**「回合结束」行(按 msg_id 去重:同回合多条 end_turn 行只算一次)。
/// 按字节读 + 完整行才解析,避免读到半个多字节字符。
fn tail_has_turn_end(
    path: &Path,
    offsets: &Mutex<HashMap<PathBuf, u64>>,
    last_id: &Mutex<Option<String>>,
) -> bool {
    let mut map = offsets.lock().unwrap();
    let start = *map.get(path).unwrap_or(&0);
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(end) = f.seek(SeekFrom::End(0)) else {
        return false;
    };
    if end <= start {
        // 文件被截断/轮换或无新增 → 偏移对齐到 end,避免负读 / 重读
        if end < start {
            map.insert(path.to_path_buf(), end);
        }
        return false;
    }
    if f.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut bytes = Vec::new();
    if f.take(end - start).read_to_end(&mut bytes).is_err() {
        map.insert(path.to_path_buf(), end);
        return false;
    }
    // 只消费到最后一个换行;半行留到下次(下次从它起点重读)
    let Some(last_nl) = bytes.iter().rposition(|&b| b == b'\n') else {
        return false; // 还没有完整行,不推进偏移
    };
    map.insert(path.to_path_buf(), start + last_nl as u64 + 1);
    drop(map); // 解析阶段不用持有偏移锁
    let complete = String::from_utf8_lossy(&bytes[..=last_nl]);
    let mut last = last_id.lock().unwrap();
    let mut fired = false;
    for line in complete.lines() {
        if let Some(id) = turn_end_msg_id(line) {
            // 同一 msg_id 的多条 end_turn 行(thinking + 多个 text)只触发一次
            if last.as_deref() != Some(id.as_str()) {
                *last = Some(id);
                fired = true;
            }
        }
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_replaces_slash_dot_underscore() {
        assert_eq!(
            encode_cc_dir_name(Path::new("/Users/x/IdeaProjects/rate-native")),
            "-Users-x-IdeaProjects-rate-native"
        );
        // `.` 与 `_` 也变 `-`,已有 `-` 保留
        assert_eq!(
            encode_cc_dir_name(Path::new("/a/Open-Design.app/demo_x")),
            "-a-Open-Design-app-demo-x"
        );
    }

    #[test]
    fn turn_end_only_for_assistant_end_turn_non_sidechain() {
        // 含 text 块(用户可见的最终消息)→ 算
        assert!(line_is_turn_end(
            r#"{"type":"assistant","isSidechain":false,"message":{"stop_reason":"end_turn","content":[{"type":"text"}]}}"#
        ));
        // 无 content 数组(异常)→ 保守当回合结束
        assert!(line_is_turn_end(
            r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#
        ));
        // thinking-only 的 end_turn → 不算(它比最终 text 早写,会重复触发)
        assert!(!line_is_turn_end(
            r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"thinking"}]}}"#
        ));
        // 中途调工具 = tool_use,不算
        assert!(!line_is_turn_end(
            r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"text"}]}}"#
        ));
        // 子 agent 回合 = sidechain,不算
        assert!(!line_is_turn_end(
            r#"{"type":"assistant","isSidechain":true,"message":{"stop_reason":"end_turn","content":[{"type":"text"}]}}"#
        ));
        // 用户/系统行不算;坏 JSON 不 panic
        assert!(!line_is_turn_end(r#"{"type":"user","message":{}}"#));
        assert!(!line_is_turn_end("not json"));
    }
}
