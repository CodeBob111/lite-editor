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
    child_pid: Option<u32>,
}

#[derive(Default)]
pub struct TerminalState {
    terminals: Mutex<HashMap<u32, TerminalInstance>>,
    next_id: AtomicU32,
}

#[tauri::command]
pub fn spawn_terminal(
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
        child_pid,
    };

    let mut terminals = state.terminals.lock().map_err(|e| e.to_string())?;
    terminals.insert(id, instance);
    Ok(id)
}

#[tauri::command]
pub fn write_terminal(
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
pub fn resize_terminal(
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
pub fn close_terminal(id: u32, state: State<'_, TerminalState>) -> Result<(), String> {
    let mut terminals = state.terminals.lock().map_err(|e| e.to_string())?;
    terminals.remove(&id);
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

#[tauri::command]
pub fn get_claude_status(id: u32, state: State<'_, TerminalState>) -> Result<Option<String>, String> {
    let shell_pid = {
        let terminals = state.terminals.lock().map_err(|e| e.to_string())?;
        match terminals.get(&id) {
            Some(inst) => match inst.child_pid {
                Some(pid) => pid,
                None => return Ok(None),
            },
            None => return Ok(None),
        }
    };

    let claude_pid = match find_claude_descendant(shell_pid) {
        Some(pid) => pid,
        None => return Ok(None),
    };

    let home = std::env::var("HOME").unwrap_or_default();
    let session_path = format!("{}/.claude/sessions/{}.json", home, claude_pid);
    let session_json = match std::fs::read_to_string(&session_path) {
        Ok(s) => s,
        Err(_) => return Ok(Some("wait".into())),
    };

    let session: serde_json::Value = match serde_json::from_str(&session_json) {
        Ok(v) => v,
        Err(_) => return Ok(Some("wait".into())),
    };

    let status = match session["status"].as_str() {
        Some("busy") => "work",
        _ => "wait",
    };

    Ok(Some(status.into()))
}
