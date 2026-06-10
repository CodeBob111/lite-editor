use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::{Emitter, State};

fn utf8_valid_prefix_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => e.valid_up_to(),
    }
}

struct TerminalInstance {
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    // 持有 shell 子进程句柄:关闭终端时 kill + wait 回收,否则每关一个终端留一个僵尸进程。
    child: Box<dyn portable_pty::Child + Send + Sync>,
    child_pid: Option<u32>,
    // 缓存上次解析到的 claude 进程 pid。仅作为快路径提示——真正的状态来源是
    // claude 的 session 文件；缓存失效时会重新走 ps 解析（见 get_claude_status）。
    claude_pid: Option<u32>,
}

#[derive(Default)]
pub struct TerminalState {
    terminals: Mutex<HashMap<u32, TerminalInstance>>,
    next_id: AtomicU32,
}

// 终端命令统一 async:同步命令在主线程执行,openpty/spawn、PTY 写满时的阻塞写
// 都不该有冻住 UI 的可能(async 后跑在 runtime 线程,最坏只占一个 worker)。
#[tauri::command]
pub async fn spawn_terminal(
    cwd: String,
    cols: u16,
    rows: u16,
    app: tauri::AppHandle,
    state: State<'_, TerminalState>,
) -> Result<u32, String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to open pty: {}", e))?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("Failed to spawn shell: {}", e))?;

    let child_pid = child.process_id();

    drop(pair.slave);

    let id = state.next_id.fetch_add(1, Ordering::Relaxed);

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to get pty writer: {}", e))?;

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to get pty reader: {}", e))?;

    let app_clone = app.clone();
    let term_id = id;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending = 0usize;
        loop {
            match reader.read(&mut buf[pending..]) {
                Ok(0) => break,
                Ok(n) => {
                    let total = pending + n;
                    let valid_end = utf8_valid_prefix_len(&buf[..total]);
                    if valid_end > 0 {
                        let data = std::str::from_utf8(&buf[..valid_end]).unwrap();
                        let _ = app_clone.emit("terminal-output", (term_id, data));
                    }
                    if valid_end < total {
                        buf.copy_within(valid_end..total, 0);
                        pending = total - valid_end;
                    } else {
                        pending = 0;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = app_clone.emit("terminal-exit", term_id);
    });

    let instance = TerminalInstance {
        writer,
        master: pair.master,
        child,
        child_pid,
        claude_pid: None,
    };

    let mut terminals = state.terminals.lock().map_err(|e| e.to_string())?;
    terminals.insert(id, instance);
    Ok(id)
}

#[tauri::command]
pub async fn write_terminal(
    id: u32,
    data: String,
    state: State<'_, TerminalState>,
) -> Result<(), String> {
    let mut terminals = state.terminals.lock().map_err(|e| e.to_string())?;
    let instance = terminals
        .get_mut(&id)
        .ok_or_else(|| format!("Terminal {} not found", id))?;
    instance
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| format!("Write failed: {}", e))?;
    instance
        .writer
        .flush()
        .map_err(|e| format!("Flush failed: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn resize_terminal(
    id: u32,
    cols: u16,
    rows: u16,
    state: State<'_, TerminalState>,
) -> Result<(), String> {
    let terminals = state.terminals.lock().map_err(|e| e.to_string())?;
    let instance = terminals
        .get(&id)
        .ok_or_else(|| format!("Terminal {} not found", id))?;
    instance
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Resize failed: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn close_terminal(id: u32, state: State<'_, TerminalState>) -> Result<(), String> {
    let inst = {
        let mut terminals = state.terminals.lock().map_err(|e| e.to_string())?;
        terminals.remove(&id)
    };
    if let Some(mut inst) = inst {
        // kill 后必须 wait 回收子进程;wait 会等 shell 真正退出,放后台线程,不卡调用方。
        std::thread::spawn(move || {
            let _ = inst.child.kill();
            let _ = inst.child.wait();
        });
    }
    Ok(())
}

fn find_claude_descendant(shell_pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-xo", "pid=,ppid=,comm="])
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    let mut children_map: HashMap<u32, Vec<(u32, String)>> = HashMap::new();

    for line in text.lines() {
        let ws_parts: Vec<&str> = line.split_whitespace().collect();
        if ws_parts.len() < 3 { continue; }
        let pid = match ws_parts[0].parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let ppid = match ws_parts[1].parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let comm = ws_parts[2..].join(" ");
        children_map.entry(ppid).or_default().push((pid, comm));
    }

    let mut queue = vec![shell_pid];
    while let Some(parent) = queue.pop() {
        if let Some(kids) = children_map.get(&parent) {
            for (pid, comm) in kids {
                let basename = comm.rsplit('/').next().unwrap_or(comm);
                if basename == "claude" || basename == "claude.exe" {
                    return Some(*pid);
                }
                queue.push(*pid);
            }
        }
    }

    None
}

/// 廉价的存活探测：进程是否还在？（不 spawn 任何子进程。）
/// EPERM 也算存活——进程存在，只是我们无权向它发信号。
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// 从 claude 的 session 文件读取状态。
/// 返回 None 表示文件缺失/不可读（调用方据此判断缓存是否已失效）；
/// 返回 Some 表示文件存在——解析失败时退化为 "wait"，与原有语义一致。
fn read_claude_status(claude_pid: u32) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let session_path = format!("{}/.claude/sessions/{}.json", home, claude_pid);
    let session_json = std::fs::read_to_string(&session_path).ok()?;
    let status = match serde_json::from_str::<serde_json::Value>(&session_json) {
        Ok(v) => match v["status"].as_str() {
            Some("busy") => "work",
            _ => "wait",
        },
        Err(_) => "wait",
    };
    Some(status.into())
}

#[tauri::command]
pub async fn get_claude_status(id: u32, state: State<'_, TerminalState>) -> Result<Option<String>, String> {
    // 1) 在锁内取出 shell_pid 与缓存的 claude_pid，随即释放锁——
    //    昂贵的 ps spawn 绝不在持锁时进行，避免阻塞 write/resize 等命令。
    let (shell_pid, cached) = {
        let terminals = state.terminals.lock().map_err(|e| e.to_string())?;
        match terminals.get(&id) {
            Some(inst) => match inst.child_pid {
                Some(pid) => (pid, inst.claude_pid),
                None => return Ok(None),
            },
            None => return Ok(None),
        }
    };

    // 2) 快路径：只有当缓存的 pid 仍存活、且其 session 文件可读时才信任缓存。
    //    session 文件是状态的唯一真相来源，缓存的 pid 只是命中提示。
    //    这样既避免了 pid 复用导致的误报，又在常态下完全省掉 ps spawn。
    if let Some(cpid) = cached {
        if pid_alive(cpid) {
            if let Some(status) = read_claude_status(cpid) {
                return Ok(Some(status));
            }
        }
        // pid 已死，或 session 文件消失 → 缓存失效，落到慢路径重新解析。
    }

    // 3) 慢路径：spawn ps 重新解析 claude 子孙进程（唯一昂贵的调用,放阻塞线程池），并回写缓存。
    let new_pid = tokio::task::spawn_blocking(move || find_claude_descendant(shell_pid))
        .await
        .map_err(|e| format!("Task failed: {}", e))?;
    {
        let mut terminals = state.terminals.lock().map_err(|e| e.to_string())?;
        if let Some(inst) = terminals.get_mut(&id) {
            inst.claude_pid = new_pid;
        }
    }
    let cpid = match new_pid {
        Some(pid) => pid,
        None => return Ok(None),
    };
    // 新解析到的 claude：报告其状态（session 文件缺失退化为 "wait"，沿用原语义）。
    Ok(Some(read_claude_status(cpid).unwrap_or_else(|| "wait".into())))
}
