//! 首启自动注册 CC「完成通知」Stop hook 到 `~/.claude/settings.json`(用户选了静默自动注册)。
//!
//! 安全:① 备份(`settings.json.nib-bak-<ts>`);② 解析合并(serde_json,非文本拼接),只往
//! `hooks.Stop` 追加自己那一条,不动别人的;③ 幂等(命令含 `nib/cc-stop-hook.sh` 标记即已注册);
//! ④ 原子写(临时文件 + rename);⑤ 解析失败直接放弃,绝不写坏。
//!
//! marker 文件保证「只在首次写一次」——用户事后手动删掉 hook 不会被重新加回(要重置删 marker)。
//! hook gated on `$NIB_TERMINAL` → 只对 Nib 终端里起的 CC 生效,与全局其它会话隔离、不依赖 cwd。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const HOOK_SCRIPT: &str = r#"#!/bin/sh
# Nib CC 完成通知 hook:只对 Nib 终端里起的 CC 会话生效(NIB_TERMINAL),把 CC 的 Stop 载荷
# (含 cwd/session_id,来自 stdin)写到 Nib 事件目录,Nib watch 该目录即提醒。非 Nib 会话瞬间退出。
[ -n "$NIB_TERMINAL" ] || exit 0
d="$HOME/Library/Application Support/nib/cc-events"
mkdir -p "$d"
cat > "$d/ev.$(date +%s).$$.json"
"#;

/// 命令里含这个子串即「Nib 自己的 hook」,用于幂等判重。
const HOOK_MARKER: &str = "nib/cc-stop-hook.sh";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Nib 的 Application Support 目录(事件目录 / 脚本 / marker 都在这)。
pub fn nib_support_dir() -> Option<PathBuf> {
    Some(home()?.join("Library/Application Support/nib"))
}

/// CC 完成事件目录(hook 写、Nib watch)。
pub fn cc_events_dir() -> Option<PathBuf> {
    Some(nib_support_dir()?.join("cc-events"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// 本次新写入了 settings.json
    Registered,
    /// settings.json 里已有 Nib 的 hook(没改)
    AlreadyPresent,
    /// marker 已存在,首次注册早做过了(没碰任何文件)
    SkippedMarker,
}

/// 首启调用:没注册过(marker 不存在)才动手——写 hook 脚本 + 安全合并进 settings.json + 写 marker。
/// 幂等;失败返回 Err 且不写 marker(下次重试)。有文件 IO,调用方应 off 主线程跑。
pub fn ensure_registered_once() -> Result<RegisterOutcome, String> {
    let support = nib_support_dir().ok_or("无 HOME")?;
    let settings = home().ok_or("无 HOME")?.join(".claude/settings.json");
    ensure_registered_at(&support, &settings)
}

/// `ensure_registered_once` 的可注入路径核心(便于用临时目录测整条 IO 流程:备份/原子写/marker)。
pub fn ensure_registered_at(support: &Path, settings: &Path) -> Result<RegisterOutcome, String> {
    let marker = support.join(".cc-hook-registered");
    if marker.exists() {
        return Ok(RegisterOutcome::SkippedMarker);
    }
    std::fs::create_dir_all(support.join("cc-events")).map_err(|e| e.to_string())?;

    // 写 hook 脚本(我们自己的文件,覆盖无妨)+ 置可执行
    let script = support.join("cc-stop-hook.sh");
    std::fs::write(&script, HOOK_SCRIPT).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    }

    let hook_command = format!("sh \"{}\"", script.display());
    let existing = std::fs::read_to_string(settings).unwrap_or_else(|_| "{}".to_string());

    match merge_stop_hook(&existing, &hook_command)? {
        None => {
            mark_done(&marker);
            Ok(RegisterOutcome::AlreadyPresent)
        }
        Some(merged) => {
            if settings.exists() {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let bak = settings.with_file_name(format!("settings.json.nib-bak-{ts}"));
                let _ = std::fs::copy(settings, &bak);
            } else if let Some(dir) = settings.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let tmp = settings.with_extension("json.nib-tmp");
            std::fs::write(&tmp, merged).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, settings).map_err(|e| e.to_string())?;
            mark_done(&marker);
            Ok(RegisterOutcome::Registered)
        }
    }
}

fn mark_done(marker: &Path) {
    let _ = std::fs::write(marker, "1");
}

/// 把 Nib 的 Stop hook 合并进 settings.json 文本。已存在(命令含 `HOOK_MARKER`)→ `Ok(None)` 不改;
/// 否则 `Ok(Some(新 JSON))`。顶层/hooks/Stop 结构类型不对 → `Err`(放弃,绝不写坏)。
pub fn merge_stop_hook(settings_json: &str, hook_command: &str) -> Result<Option<String>, String> {
    let trimmed = settings_json.trim();
    let mut root: Value = if trimmed.is_empty() {
        json!({})
    } else {
        serde_json::from_str(trimmed).map_err(|e| format!("settings.json 解析失败: {e}"))?
    };
    let obj = root.as_object_mut().ok_or("settings.json 顶层不是对象")?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks 不是对象")?;
    let stop = hooks_obj.entry("Stop").or_insert_with(|| json!([]));
    let stop_arr = stop.as_array_mut().ok_or("hooks.Stop 不是数组")?;
    if stop_arr.iter().any(|e| entry_has_marker(e, HOOK_MARKER)) {
        return Ok(None);
    }
    stop_arr.push(json!({
        "hooks": [ { "type": "command", "command": hook_command, "async": true } ]
    }));
    Ok(Some(
        serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?,
    ))
}

fn entry_has_marker(entry: &Value, marker: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains(marker))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "sh \"/Users/x/Library/Application Support/nib/cc-stop-hook.sh\"";

    #[test]
    fn merge_into_empty_creates_stop_hook() {
        let out = merge_stop_hook("", CMD).unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let stop = &v["hooks"]["Stop"];
        assert_eq!(stop.as_array().unwrap().len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], CMD);
        assert_eq!(stop[0]["hooks"][0]["async"], true);
    }

    #[test]
    fn merge_keeps_existing_hooks_and_appends() {
        let existing = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"other-tool"}]}],"PreToolUse":[{"hooks":[]}]}}"#;
        let out = merge_stop_hook(existing, CMD).unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        // 原有的 other-tool 保留 + 追加 Nib 的 → 2 条
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "other-tool");
        assert_eq!(stop[1]["hooks"][0]["command"], CMD);
        // 别的事件不受影响
        assert!(v["hooks"]["PreToolUse"].is_array());
    }

    #[test]
    fn merge_is_idempotent() {
        let first = merge_stop_hook("{}", CMD).unwrap().unwrap();
        // 第二次合并(已含标记)→ None,不重复加
        assert_eq!(merge_stop_hook(&first, CMD).unwrap(), None);
    }

    #[test]
    fn merge_aborts_on_malformed() {
        assert!(merge_stop_hook("{ not json", CMD).is_err());
        // 类型不对(Stop 是对象不是数组)也放弃
        assert!(merge_stop_hook(r#"{"hooks":{"Stop":{}}}"#, CMD).is_err());
    }

    #[test]
    fn ensure_registered_at_full_flow_with_tempdirs() {
        // 用唯一临时目录隔离,跑整条 IO:备份 / 原子写 / marker / 幂等 / marker 短路
        let base = std::env::temp_dir().join(format!(
            "nib-cc-hook-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let support = base.join("support");
        let claude = base.join("claude");
        std::fs::create_dir_all(&support).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        let settings = claude.join("settings.json");
        std::fs::write(
            &settings,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"masko"}]}]}}"#,
        )
        .unwrap();

        // 首次:注册 + 备份 + marker
        assert_eq!(
            ensure_registered_at(&support, &settings).unwrap(),
            RegisterOutcome::Registered
        );
        let merged: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2); // 原 masko 保留 + Nib 追加
        assert_eq!(stop[0]["hooks"][0]["command"], "masko");
        assert!(support.join(".cc-hook-registered").exists());
        assert!(support.join("cc-stop-hook.sh").exists());
        // 备份存在
        assert!(std::fs::read_dir(&claude)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("nib-bak")));

        // 第二次:marker 在 → 短路,不再动 settings.json
        assert_eq!(
            ensure_registered_at(&support, &settings).unwrap(),
            RegisterOutcome::SkippedMarker
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
