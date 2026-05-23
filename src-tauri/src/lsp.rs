use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use tauri::State;

// ---- Public state ----

#[derive(Default)]
pub struct LspState {
    servers: Mutex<HashMap<String, LspServer>>,
}

struct LspServer {
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicI64,
    #[allow(dead_code)]
    root_uri: String,
}

// ---- Tauri commands ----

#[derive(Serialize, Clone)]
pub struct LspUsage {
    uri: String,
    line: u32,
    character: u32,
    text: String,
}

#[tauri::command]
pub fn start_lsp(
    language: String,
    root_path: String,
    state: State<'_, LspState>,
) -> Result<(), String> {
    let mut servers = state.servers.lock().map_err(|e| e.to_string())?;

    if servers.contains_key(&language) {
        return Ok(());
    }

    let (cmd, args): (&str, Vec<&str>) = match language.as_str() {
        "python" => ("pyright-langserver", vec!["--stdio"]),
        "typescript" | "javascript" => ("typescript-language-server", vec!["--stdio"]),
        "java" => ("jdtls", vec![]),
        _ => return Err(format!("Unsupported language: {}", language)),
    };

    let mut process = Command::new(cmd)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start {} ({}): {}. Is it installed?", cmd, language, e))?;

    let stdin = process.stdin.take().ok_or("Failed to capture LSP stdin")?;
    let stdout = process.stdout.take().ok_or("Failed to capture LSP stdout")?;

    let root_uri = format!("file://{}", root_path);

    let server = LspServer {
        stdin: Mutex::new(stdin),
        stdout: Mutex::new(BufReader::new(stdout)),
        next_id: AtomicI64::new(2), // 1 used for initialize
        root_uri: root_uri.clone(),
    };

    // Send initialize request
    let init_params = serde_json::json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "references": { "dynamicRegistration": false },
                "definition": { "dynamicRegistration": false },
                "synchronization": {
                    "didSave": true,
                    "willSave": false,
                    "willSaveWaitUntil": false
                }
            }
        },
        "workspaceFolders": [{
            "uri": root_uri,
            "name": root_path.split('/').last().unwrap_or("project")
        }]
    });

    send_request(&server, 1, "initialize", init_params)?;
    let _init_result = read_response(&server)?;

    // Send initialized notification
    send_notification(&server, "initialized", serde_json::json!({}))?;

    servers.insert(language, server);
    Ok(())
}

#[tauri::command]
pub fn stop_lsp(language: String, state: State<'_, LspState>) -> Result<(), String> {
    let mut servers = state.servers.lock().map_err(|e| e.to_string())?;
    if let Some(server) = servers.remove(&language) {
        let id = server.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = send_request(&server, id, "shutdown", serde_json::Value::Null);
        let _ = send_notification(&server, "exit", serde_json::Value::Null);
        // stdin/stdout drop here, process will be cleaned up
    }
    Ok(())
}

#[tauri::command]
pub fn lsp_did_open(
    file_path: String,
    language_id: String,
    content: String,
    state: State<'_, LspState>,
) -> Result<(), String> {
    let servers = state.servers.lock().map_err(|e| e.to_string())?;
    let server = find_server(&servers, &language_id)?;

    let params = serde_json::json!({
        "textDocument": {
            "uri": format!("file://{}", file_path),
            "languageId": language_id,
            "version": 1,
            "text": content
        }
    });

    send_notification(server, "textDocument/didOpen", params)
}

#[tauri::command]
pub fn lsp_did_change(
    file_path: String,
    content: String,
    state: State<'_, LspState>,
) -> Result<(), String> {
    let servers = state.servers.lock().map_err(|e| e.to_string())?;
    let lang = detect_language(&file_path);
    let server = match find_server(&servers, &lang) {
        Ok(s) => s,
        Err(_) => return Ok(()), // No server for this language, silently skip
    };

    let params = serde_json::json!({
        "textDocument": {
            "uri": format!("file://{}", file_path),
            "version": 2
        },
        "contentChanges": [{ "text": content }]
    });
    send_notification(server, "textDocument/didChange", params)
}

#[tauri::command]
pub fn lsp_find_references(
    file_path: String,
    line: u32,
    character: u32,
    state: State<'_, LspState>,
) -> Result<Vec<LspUsage>, String> {
    let servers = state.servers.lock().map_err(|e| e.to_string())?;
    let lang = detect_language(&file_path);
    let server = find_server(&servers, &lang)?;

    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    let params = serde_json::json!({
        "textDocument": { "uri": format!("file://{}", file_path) },
        "position": { "line": line, "character": character },
        "context": { "includeDeclaration": true }
    });

    send_request(server, id, "textDocument/references", params)?;
    let response = read_response(server)?;

    parse_locations(response)
}

#[tauri::command]
pub fn lsp_goto_definition(
    file_path: String,
    line: u32,
    character: u32,
    state: State<'_, LspState>,
) -> Result<Option<LspUsage>, String> {
    let servers = state.servers.lock().map_err(|e| e.to_string())?;
    let lang = detect_language(&file_path);
    let server = find_server(&servers, &lang)?;

    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    let params = serde_json::json!({
        "textDocument": { "uri": format!("file://{}", file_path) },
        "position": { "line": line, "character": character }
    });

    send_request(server, id, "textDocument/definition", params)?;
    let response = read_response(server)?;

    let locations = parse_locations(response)?;
    Ok(locations.into_iter().next())
}

// ---- Internals ----

fn find_server<'a>(
    servers: &'a HashMap<String, LspServer>,
    language: &str,
) -> Result<&'a LspServer, String> {
    let key = match language {
        "python" | "py" => "python",
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" => "typescript",
        "java" => "java",
        other => other,
    };
    servers
        .get(key)
        .ok_or_else(|| format!("No LSP server running for '{}'. Start one first.", key))
}

fn detect_language(path: &str) -> String {
    if path.ends_with(".py") {
        "python".into()
    } else if path.ends_with(".ts") || path.ends_with(".tsx") {
        "typescript".into()
    } else if path.ends_with(".js") || path.ends_with(".jsx") {
        "javascript".into()
    } else if path.ends_with(".java") {
        "java".into()
    } else {
        "unknown".into()
    }
}

fn send_request(server: &LspServer, id: i64, method: &str, params: serde_json::Value) -> Result<(), String> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    send_message(server, &msg)
}

fn send_notification(server: &LspServer, method: &str, params: serde_json::Value) -> Result<(), String> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    });
    send_message(server, &msg)
}

fn send_message(server: &LspServer, msg: &serde_json::Value) -> Result<(), String> {
    let body = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());

    let mut stdin = server.stdin.lock().map_err(|e| e.to_string())?;
    stdin
        .write_all(header.as_bytes())
        .map_err(|e| format!("Failed to write to LSP stdin: {}", e))?;
    stdin
        .write_all(body.as_bytes())
        .map_err(|e| format!("Failed to write to LSP stdin: {}", e))?;
    stdin
        .flush()
        .map_err(|e| format!("Failed to flush LSP stdin: {}", e))?;
    Ok(())
}

fn read_response(server: &LspServer) -> Result<serde_json::Value, String> {
    let mut reader = server.stdout.lock().map_err(|e| e.to_string())?;

    // Loop to skip server-initiated notifications (they lack "id")
    loop {
        // Read headers
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|e| format!("Failed to read LSP header: {}", e))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len_str
                    .parse()
                    .map_err(|e| format!("Invalid Content-Length: {}", e))?;
            }
        }

        if content_length == 0 {
            return Err("Missing Content-Length header in LSP response".into());
        }

        // Read body
        let mut body = vec![0u8; content_length];
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("Failed to read LSP body: {}", e))?;

        let response: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| format!("Invalid JSON from LSP: {}", e))?;

        // Skip notifications (no "id" field) — keep reading
        if response.get("id").is_none() {
            continue;
        }

        // Check for error
        if let Some(error) = response.get("error") {
            return Err(format!("LSP error: {}", error));
        }

        return Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null));
    }
}

fn parse_locations(result: serde_json::Value) -> Result<Vec<LspUsage>, String> {
    let mut usages = Vec::new();

    let locations = match &result {
        serde_json::Value::Array(arr) => arr.clone(),
        serde_json::Value::Object(_) => vec![result],
        serde_json::Value::Null => return Ok(usages),
        _ => return Err("Unexpected LSP response format".into()),
    };

    for loc in locations {
        let uri = loc
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let range = loc.get("range").cloned().unwrap_or_default();
        let start = range.get("start").cloned().unwrap_or_default();
        let line = start.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let character = start
            .get("character")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        // Try to read the source line for context
        let file_path = uri.strip_prefix("file://").unwrap_or(&uri);
        let text = read_source_line(file_path, line);

        usages.push(LspUsage {
            uri,
            line,
            character,
            text,
        });
    }

    Ok(usages)
}

fn read_source_line(path: &str, line: u32) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| {
            content
                .lines()
                .nth(line as usize)
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}
