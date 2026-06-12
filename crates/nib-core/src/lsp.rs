use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::path::PathBuf;
use std::time::Duration;

use crate::events::{CoreEvent, EventSink};

#[derive(Default)]
pub struct LspState {
    servers: Mutex<HashMap<(String, String), Arc<LspServer>>>,
    /// 串行化 start_lsp 全程(检查→慢启动→插入):没有它,并发打开两个
    /// Java 文件会双开 jdtls,第二个 insert 覆盖第一个 → 进程孤儿化
    start_lock: tokio::sync::Mutex<()>,
}

impl LspState {
    // 应用退出时必须显式调用(见 lib.rs 的 RunEvent::Exit):Tauri 在事件循环结束后
    // 直接 process::exit,托管状态的 Drop 不会执行;jdtls 不随父进程退出(实证:父进程
    // 被杀后 ppid=1 仍存活),不杀干净的话每次退出都孤儿化一批 ~1.5G 堆的 JVM。
    pub fn kill_all(&self) {
        if let Ok(mut servers) = self.servers.lock() {
            for (_, server) in servers.drain() {
                if let Ok(mut process) = server.process.lock() {
                    let _ = process.kill();
                    let _ = process.wait();
                }
            }
        }
    }
}

impl Drop for LspState {
    fn drop(&mut self) {
        self.kill_all();
    }
}

struct LspServer {
    stdin: Arc<Mutex<std::io::BufWriter<ChildStdin>>>,
    response_rx: Mutex<mpsc::Receiver<serde_json::Value>>,
    next_id: AtomicI64,
    #[allow(dead_code)]
    root_uri: String,
    process: Mutex<Child>,
    ready: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct LspUsage {
    pub uri: String,
    pub line: u32,
    pub character: u32,
    pub text: String,
}

// ---- Tauri commands ----

pub async fn start_lsp(
    language: String,
    root_path: String,
    events: Arc<dyn EventSink>,
    jdtls_root: PathBuf,
    state: &LspState,
) -> Result<(), String> {
    let key = (language.clone(), root_path.clone());
    let _start_guard = state.start_lock.lock().await;

    {
        let mut servers = state.servers.lock().map_err(|e| e.to_string())?;

        if let Some(server) = servers.get(&key) {
            let mut process = server.process.lock().map_err(|e| e.to_string())?;
            if let Ok(None) = process.try_wait() {
                return Ok(());
            }
            drop(process);
        }
        servers.remove(&key);
    }

    let lang = language.clone();
    let rp = root_path.clone();
    let server = crate::rt::spawn_blocking(move || {
        start_lsp_blocking(lang, rp, events, jdtls_root)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))??;

    let mut servers = state.servers.lock().map_err(|e| e.to_string())?;
    servers.insert(key, Arc::new(server));
    Ok(())
}

/// Finder/Dock 启动的 macOS app 拿到的是 launchd 的最小 PATH(实测为空,仅回落
/// `/usr/bin:/bin:/usr/sbin:/sbin`),**不含 `/opt/homebrew/bin`** —— jdtls /
/// pyright-langserver / typescript-language-server(及后两者依赖的 node)都装在那里,
/// 于是裸 `Command::new("jdtls")` 在装机后从 Dock 启动时 spawn 失败 NotFound,
/// server 永不入 map,跳转/查引用全报 "No LSP server"。dev 时 `cargo run` 从带
/// homebrew PATH 的 shell 跑所以一直没暴露。这里把常见包管理器 bin 目录补进子进程 PATH。
fn augmented_path() -> String {
    let mut dirs: Vec<String> = vec![
        "/opt/homebrew/bin".into(),
        "/opt/homebrew/sbin".into(),
        "/usr/local/bin".into(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/.local/bin"));
    }
    dirs.push(
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
    );
    dirs.join(":")
}

fn start_lsp_blocking(
    language: String,
    root_path: String,
    events: Arc<dyn EventSink>,
    jdtls_root: PathBuf,
) -> Result<LspServer, String> {
    let jdtls_data_dir;
    let (cmd, args): (&str, Vec<String>) = match language.as_str() {
        "python" => ("pyright-langserver", vec!["--stdio".into()]),
        "typescript" | "javascript" => ("typescript-language-server", vec!["--stdio".into()]),
        "java" => {
            let hash = root_path.replace('/', "_");
            jdtls_data_dir = jdtls_root.join(hash).to_string_lossy().to_string();
            let _ = std::fs::create_dir_all(&jdtls_data_dir);
            ("jdtls", vec![
                "--jvm-arg=-Xmx1536m".into(),
                "-data".into(),
                jdtls_data_dir.clone(),
            ])
        }
        _ => return Err(format!("Unsupported language: {}", language)),
    };

    let mut command = Command::new(cmd);
    command
        .args(&args)
        .env("PATH", augmented_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    if language == "java" {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                Ok(())
            });
        }
    }

    let mut process = command.spawn().map_err(|e| {
        format!(
            "Failed to start {} ({}): {}. Is it installed?",
            cmd, language, e
        )
    })?;

    let stdin = process.stdin.take().ok_or("Failed to capture LSP stdin")?;
    let stdout = process
        .stdout
        .take()
        .ok_or("Failed to capture LSP stdout")?;

    if let Some(stderr) = process.stderr.take() {
        let lang_for_log = language.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let log_path = format!("/tmp/{}-stderr.log", lang_for_log);
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                use std::io::Write;
                for line in reader.lines().map_while(Result::ok) {
                    let _ = writeln!(file, "{}", line);
                }
            }
        });
    }

    let root_uri = format!("file://{}", root_path);

    let (tx, rx) = mpsc::channel::<serde_json::Value>();

    let ready_flag = Arc::new(AtomicBool::new(false));
    let ready_clone = ready_flag.clone();
    let lang_clone = language.clone();
    let events_for_reader = events;
    let stdin_for_reader = Arc::new(Mutex::new(std::io::BufWriter::new(stdin)));
    let stdin_for_server = stdin_for_reader.clone();

    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(msg) = read_next_message(&mut reader) {
            {
                    let has_id = msg.get("id").is_some();
                    let method = msg.get("method").and_then(|m| m.as_str()).map(String::from);

                    if has_id && method.is_some() {
                        if let Some(id) = msg.get("id") {
                            let method_str = method.as_deref().unwrap_or("");
                            let result = match method_str {
                                "workspace/configuration" => {
                                    let items_arr = msg
                                        .get("params")
                                        .and_then(|p| p.get("items"))
                                        .and_then(|i| i.as_array());
                                    let home =
                                        std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                                    let maven_settings = format!("{}/.m2/settings.xml", home);
                                    let java_settings = serde_json::json!({
                                        "import": {
                                            "maven": { "enabled": true },
                                            "gradle": { "enabled": true }
                                        },
                                        "autobuild": { "enabled": true },
                                        "configuration": {
                                            "updateBuildConfiguration": "automatic",
                                            "maven": {
                                                "userSettings": maven_settings
                                            }
                                        },
                                        "maven": {
                                            "downloadSources": true
                                        },
                                        "referencesCodeLens": { "enabled": false },
                                        "implementationsCodeLens": { "enabled": false }
                                    });
                                    let items: Vec<serde_json::Value> = if let Some(arr) = items_arr
                                    {
                                        arr.iter()
                                            .map(|item| {
                                                let section = item
                                                    .get("section")
                                                    .and_then(|s| s.as_str())
                                                    .unwrap_or("");
                                                if section == "java" || section.starts_with("java.")
                                                {
                                                    java_settings.clone()
                                                } else {
                                                    serde_json::json!({})
                                                }
                                            })
                                            .collect()
                                    } else {
                                        vec![java_settings]
                                    };
                                    serde_json::Value::Array(items)
                                }
                                _ => serde_json::Value::Null,
                            };
                            let resp = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": result
                            });
                            let body = serde_json::to_string(&resp).unwrap_or_default();
                            let header = format!("Content-Length: {}\r\n\r\n", body.len());
                            if let Ok(mut w) = stdin_for_reader.lock() {
                                use std::io::Write;
                                let _ = w.write_all(header.as_bytes());
                                let _ = w.write_all(body.as_bytes());
                                let _ = w.flush();
                            }
                        }
                    } else if has_id {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    } else if let Some(method) = method {
                        match method.as_str() {
                            "textDocument/publishDiagnostics" => {
                                if let Some(params) = msg.get("params") {
                                    events_for_reader
                                        .emit(CoreEvent::LspDiagnostics(params.clone()));
                                }
                            }
                            "$/progress" => {
                                if let Some(params) = msg.get("params") {
                                    let value = params.get("value").cloned().unwrap_or_default();
                                    let kind =
                                        value.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                    let message =
                                        value.get("message").and_then(|m| m.as_str()).unwrap_or("");
                                    let percentage =
                                        value.get("percentage").and_then(|p| p.as_u64());

                                    events_for_reader.emit(CoreEvent::LspProgress {
                                        language: lang_clone.clone(),
                                        kind: kind.to_string(),
                                        message: message.to_string(),
                                        percentage,
                                    });

                                    if kind == "end" {
                                        ready_clone.store(true, Ordering::Relaxed);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
            }
        }
    });

    let server = LspServer {
        stdin: stdin_for_server,
        response_rx: Mutex::new(rx),
        next_id: AtomicI64::new(2),
        root_uri: root_uri.clone(),
        process: Mutex::new(process),
        ready: ready_flag,
    };

    let init_params = serde_json::json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "references": { "dynamicRegistration": false },
                "definition": { "dynamicRegistration": false },
                "documentSymbol": {
                    "dynamicRegistration": false,
                    "hierarchicalDocumentSymbolSupport": true
                },
                "publishDiagnostics": { "relatedInformation": false },
                "synchronization": {
                    "didSave": true,
                    "willSave": false,
                    "willSaveWaitUntil": false
                }
            },
            "workspace": {
                "configuration": true,
                "didChangeConfiguration": { "dynamicRegistration": true },
                "workspaceFolders": true
            },
            "window": {
                "workDoneProgress": true
            }
        },
        "initializationOptions": if language == "java" {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let maven_settings = format!("{}/.m2/settings.xml", home);
            serde_json::json!({
                "settings": {
                    "java": {
                        "import": {
                            "maven": { "enabled": true },
                            "gradle": { "enabled": true }
                        },
                        "autobuild": { "enabled": true },
                        "configuration": {
                            "updateBuildConfiguration": "automatic",
                            "maven": {
                                "userSettings": maven_settings
                            }
                        },
                        "maven": {
                            "downloadSources": true
                        }
                    }
                },
                // 跳转定义落在依赖 jar 的 .class 时,jdtls 只有在客户端声明此能力才会
                // 返回 jdt://contents/... 位置(否则直接返回空 → "未找到定义")。
                // 拿到该 URI 后需用 java/classFileContents 自定义请求取(反编译)文本。
                "extendedClientCapabilities": {
                    "classFileContentsSupport": true
                }
            })
        } else {
            serde_json::json!({})
        },
        "workspaceFolders": build_workspace_folders(&root_path, &root_uri)
    });

    // initialize 超时放宽到 90s:大型多模块工程 + 机器繁忙时 jdtls 应答可能慢,
    // 超时会让 start_lsp 返回 Err→server 不入 map→"No LSP server"(进程却在跑)。
    // 配合切项目时 stop_lsp(避免多实例抢 CPU),正常情况下应答其实很快。
    //
    // 关键:initialize/initialized 失败时必须先杀掉已 spawn 的 jdtls 再返回 Err。
    // 否则 server 随错误被 drop,而 Rust 的 Child::drop 不杀进程 → jdtls 变孤儿
    // (空转吃 CPU、还不在 map 里没法 stop_lsp 回收),正是孤儿累积的来源之一。
    let kill_on_err = |server: &LspServer| {
        if let Ok(mut proc) = server.process.lock() {
            let _ = proc.kill();
            let _ = proc.wait();
        }
    };
    if let Err(e) = request_and_wait(
        &server,
        1,
        "initialize",
        init_params,
        Duration::from_secs(90),
    ) {
        kill_on_err(&server);
        return Err(e);
    }
    if let Err(e) = send_notification(&server, "initialized", serde_json::json!({})) {
        kill_on_err(&server);
        return Err(e);
    }

    if language != "java" {
        server.ready.store(true, Ordering::Relaxed);
    }

    Ok(server)
}

pub async fn stop_lsp(
    language: String,
    root_path: String,
    state: &LspState,
) -> Result<(), String> {
    let server = {
        let mut servers = state.servers.lock().map_err(|e| e.to_string())?;
        servers.remove(&(language, root_path))
    };
    if let Some(server) = server {
        // shutdown 握手最长等 3s:放到阻塞线程池,不能让调用方(主线程)跟着等。
        let _ = crate::rt::spawn_blocking(move || {
            let id = server.next_id.fetch_add(1, Ordering::Relaxed);
            let _ = request_and_wait(
                &server,
                id,
                "shutdown",
                serde_json::Value::Null,
                Duration::from_secs(3),
            );
            let _ = send_notification(&server, "exit", serde_json::Value::Null);
            if let Ok(mut proc) = server.process.lock() {
                let _ = proc.kill();
                let _ = proc.wait(); // kill 后必须 wait 回收,否则留僵尸进程
            }
        })
        .await;
    }
    Ok(())
}

pub fn lsp_is_ready(file_path: String, state: &LspState) -> Result<bool, String> {
    let lang = detect_language(&file_path);
    let server = {
        let servers = state.servers.lock().map_err(|e| e.to_string())?;
        match find_server_for_file(&servers, &file_path, &lang) {
            Ok(s) => s,
            Err(_) => return Ok(false),
        }
    };
    Ok(server.ready.load(Ordering::Relaxed))
}

// didOpen/didChange 往 LSP stdin 写整份文件内容:jdtls 忙(索引中)不读管道时,
// 64K 管道缓冲一满 write 就阻塞——必须离开主线程。
pub async fn lsp_did_open(
    file_path: String,
    language_id: String,
    content: String,
    state: &LspState,
) -> Result<(), String> {
    let server = {
        let servers = state.servers.lock().map_err(|e| e.to_string())?;
        find_server_for_file(&servers, &file_path, &language_id)?
    };

    crate::rt::spawn_blocking(move || {
        let params = serde_json::json!({
            "textDocument": {
                "uri": format!("file://{}", file_path),
                "languageId": language_id,
                "version": 1,
                "text": content
            }
        });
        send_notification(&server, "textDocument/didOpen", params)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

pub async fn lsp_did_change(
    file_path: String,
    content: String,
    state: &LspState,
) -> Result<(), String> {
    let lang = detect_language(&file_path);
    let server = {
        let servers = state.servers.lock().map_err(|e| e.to_string())?;
        match find_server_for_file(&servers, &file_path, &lang) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        }
    };

    crate::rt::spawn_blocking(move || {
        let params = serde_json::json!({
            "textDocument": {
                "uri": format!("file://{}", file_path),
                "version": 2
            },
            "contentChanges": [{ "text": content }]
        });
        send_notification(&server, "textDocument/didChange", params)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

pub async fn lsp_find_references(
    file_path: String,
    line: u32,
    character: u32,
    state: &LspState,
) -> Result<Vec<LspUsage>, String> {
    let lang = detect_language(&file_path);
    let server = {
        let servers = state.servers.lock().map_err(|e| e.to_string())?;
        find_server_for_file(&servers, &file_path, &lang)?
    };

    let character = snap_to_identifier(&file_path, line, character);
    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    let params = serde_json::json!({
        "textDocument": { "uri": format!("file://{}", file_path) },
        "position": { "line": line, "character": character },
        "context": { "includeDeclaration": true }
    });

    let response = request_and_wait_on_worker(
        server,
        id,
        "textDocument/references",
        params,
        Duration::from_secs(5),
    )
    .await?;
    parse_locations(response)
}

pub async fn lsp_goto_definition(
    file_path: String,
    line: u32,
    character: u32,
    state: &LspState,
) -> Result<Option<LspUsage>, String> {
    dbg_log(&format!("[jdtls-def] 请求 file={file_path} line={line} char={character}"));
    let lang = detect_language(&file_path);
    let server = {
        let servers = state.servers.lock().map_err(|e| e.to_string())?;
        match find_server_for_file(&servers, &file_path, &lang) {
            Ok(s) => s,
            Err(e) => {
                dbg_log(&format!("[jdtls-def] 没有可用 server: {e}"));
                return Err(e);
            }
        }
    };

    let character = snap_to_identifier(&file_path, line, character);
    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    let params = serde_json::json!({
        "textDocument": { "uri": format!("file://{}", file_path) },
        "position": { "line": line, "character": character }
    });

    let response = request_and_wait_on_worker(
        server,
        id,
        "textDocument/definition",
        params,
        Duration::from_secs(4),
    )
    .await?;
    dbg_log(&format!(
        "[jdtls-def] line={line} char={character} 原始响应: {response}"
    ));
    let locations = parse_locations(response)?;
    Ok(locations.into_iter().next())
}

/// 返回整份文档的符号树(textDocument/documentSymbol)的原始 LSP result,
/// 前端据此按 range 命中找到光标所在的方法(像 IDEA 用 PSI 那样结构化解析,
/// 不靠正则猜方法名)。jdtls 返回层级化的 DocumentSymbol[]。
pub async fn lsp_document_symbols(
    file_path: String,
    state: &LspState,
) -> Result<serde_json::Value, String> {
    let lang = detect_language(&file_path);
    let server = {
        let servers = state.servers.lock().map_err(|e| e.to_string())?;
        find_server_for_file(&servers, &file_path, &lang)?
    };

    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    let params = serde_json::json!({
        "textDocument": { "uri": format!("file://{}", file_path) }
    });

    request_and_wait_on_worker(
        server,
        id,
        "textDocument/documentSymbol",
        params,
        Duration::from_secs(3),
    )
    .await
}

// ---- Internals ----

fn build_workspace_folders(root_path: &str, root_uri: &str) -> serde_json::Value {
    let root = std::path::Path::new(root_path);
    let has_root_pom = root.join("pom.xml").exists() || root.join("build.gradle").exists();
    if has_root_pom {
        return serde_json::json!([{
            "uri": root_uri,
            "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("project")
        }]);
    }
    let mut folders = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && (path.join("pom.xml").exists() || path.join("build.gradle").exists())
            {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project");
                let uri = format!("file://{}", path.display());
                folders.push(serde_json::json!({ "uri": uri, "name": name }));
            }
        }
    }
    if folders.is_empty() {
        serde_json::json!([{
            "uri": root_uri,
            "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("project")
        }])
    } else {
        serde_json::Value::Array(folders)
    }
}

fn find_server_for_file(
    servers: &HashMap<(String, String), Arc<LspServer>>,
    file_path: &str,
    language: &str,
) -> Result<Arc<LspServer>, String> {
    let key_lang = match language {
        "python" | "py" => "python",
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" => "typescript",
        "java" => "java",
        other => other,
    };

    for ((lang, root), server) in servers.iter() {
        if lang == key_lang {
            let root_path = root.as_str();
            if file_path.starts_with(root_path) {
                if let Ok(mut proc) = server.process.lock() {
                    if let Ok(Some(_)) = proc.try_wait() {
                        continue;
                    }
                }
                return Ok(Arc::clone(server));
            }
        }
    }

    Err(format!(
        "No LSP server running for '{}' in project containing '{}'",
        key_lang, file_path
    ))
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// cmd+click 的落点常被取整到标识符的**右边界外一格**(紧跟的 `(`、`.`、空格等),
/// jdtls 严格按位置取符号,在非标识符字符上返回空(IDEA 则会取相邻 token)。
/// 这里把光标"吸附"回标识符内:当前列不是标识符字符、而左邻是,就退一列。
/// character 是字符列(0-based,组件实证为字符非 UTF-16),与按行字符遍历一致。
fn snap_to_identifier(file_path: &str, line: u32, character: u32) -> u32 {
    if character == 0 {
        return character;
    }
    let Ok(content) = std::fs::read_to_string(file_path) else {
        return character;
    };
    let Some(line_text) = content.lines().nth(line as usize) else {
        return character;
    };
    let chars: Vec<char> = line_text.chars().collect();
    let at = character as usize;
    let cur_is_ident = chars.get(at).copied().map(is_ident_char).unwrap_or(false);
    let prev_is_ident = chars.get(at - 1).copied().map(is_ident_char).unwrap_or(false);
    if !cur_is_ident && prev_is_ident {
        character - 1
    } else {
        character
    }
}

/// 文本启发式跳转定义(不依赖 jdtls / 索引)——移植自重构前的 tryNavigateToReceiverType。
/// jdtls 返回空 / 超时 / 未就绪时的兜底:解析光标处的 `receiver.method` 或裸标识符,
/// 经 import / 同包解析出类型 FQN,在工程文件列表里按 FQN 找到 .java,再 grep 方法声明行。
/// 返回 0-based 行的 LspUsage(与 LSP 一致),失败 None。文件读放 worker,不卡主线程。
pub async fn text_fallback_definition(
    file_path: String,
    line: u32,
    character: u32,
    project_files: Vec<String>,
) -> Option<LspUsage> {
    crate::rt::on_worker(move || {
        Ok(text_fallback_definition_blocking(
            &file_path,
            line,
            character,
            &project_files,
        ))
    })
    .await
    .ok()
    .flatten()
}

fn dbg_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/nib-goto.log")
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn text_fallback_definition_blocking(
    file_path: &str,
    line: u32,
    character: u32,
    project_files: &[String],
) -> Option<LspUsage> {
    let Ok(content) = std::fs::read_to_string(file_path) else {
        dbg_log(&format!("[fallback] 读不了文件 {file_path}"));
        return None;
    };
    let Some(line_text) = content.lines().nth(line as usize).map(|s| s.to_string()) else {
        dbg_log(&format!("[fallback] 没有第 {line} 行"));
        return None;
    };
    let Some((receiver, word)) = word_and_receiver(&line_text, character as usize) else {
        dbg_log(&format!("[fallback] 光标处取不出标识符 line={line} char={character} 文本=[{line_text}]"));
        return None;
    };
    dbg_log(&format!(
        "[fallback] line={line} char={character} project_files={} receiver={receiver:?} word={word}",
        project_files.len()
    ));

    let uppercase = |s: &str| s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);

    // 1) receiver.method:解析接收者类型 → FQN → 文件 → grep 方法声明
    if let Some(recv) = &receiver {
        let type_name = if uppercase(recv) {
            recv.clone()
        } else {
            match resolve_var_type(&content, recv) {
                Some(t) => t,
                None => {
                    dbg_log(&format!("[fallback] 推不出变量 {recv} 的类型"));
                    return None;
                }
            }
        };
        let Some(fqn) = resolve_fqn(&content, &type_name) else {
            dbg_log(&format!("[fallback] 解析不出 {type_name} 的 FQN(无 import 且无 package)"));
            return None;
        };
        let rel = format!("/{}.java", fqn.replace('.', "/"));
        let Some(target) = project_files.iter().find(|p| p.ends_with(&rel)) else {
            dbg_log(&format!("[fallback] 工程文件列表里找不到后缀 {rel}(type={type_name} fqn={fqn})"));
            return None;
        };
        let l = find_method_line(target, &word).unwrap_or(1);
        dbg_log(&format!("[fallback] ✓ {type_name}.{word} → {target}:{l}"));
        return Some(LspUsage {
            uri: format!("file://{}", target),
            line: (l.saturating_sub(1)) as u32,
            character: 0,
            text: String::new(),
        });
    }

    // 2) 裸标识符:大写=类型(跳到该类文件首行),小写=同文件方法定义
    if uppercase(&word) {
        let Some(fqn) = resolve_fqn(&content, &word) else {
            dbg_log(&format!("[fallback] 裸类型 {word} 解析不出 FQN"));
            return None;
        };
        let rel = format!("/{}.java", fqn.replace('.', "/"));
        let Some(target) = project_files.iter().find(|p| p.ends_with(&rel)) else {
            dbg_log(&format!("[fallback] 裸类型找不到文件后缀 {rel}"));
            return None;
        };
        dbg_log(&format!("[fallback] ✓ 类型 {word} → {target}"));
        Some(LspUsage {
            uri: format!("file://{}", target),
            line: 0,
            character: 0,
            text: String::new(),
        })
    } else {
        let Some(l) = find_method_def_in_content(&content, &word, line as usize) else {
            dbg_log(&format!("[fallback] 同文件找不到方法 {word} 的定义"));
            return None;
        };
        dbg_log(&format!("[fallback] ✓ 同文件方法 {word} → 行 {l}"));
        Some(LspUsage {
            uri: format!("file://{}", file_path),
            line: l,
            character: 0,
            text: String::new(),
        })
    }
}

/// 取光标处的 (接收者, 标识符):标识符前紧跟 `.` 则其前的词为接收者。
fn word_and_receiver(line_text: &str, col: usize) -> Option<(Option<String>, String)> {
    let chars: Vec<char> = line_text.chars().collect();
    let col = col.min(chars.len());
    let mut start = col;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word: String = chars[start..end].iter().collect();
    if start >= 1 && chars[start - 1] == '.' {
        let mut rs = start - 1;
        while rs > 0 && is_ident_char(chars[rs - 1]) {
            rs -= 1;
        }
        let receiver: String = chars[rs..start - 1].iter().collect();
        if !receiver.is_empty() {
            return Some((Some(receiver), word));
        }
    }
    Some((None, word))
}

/// 找局部变量声明 `Type var` 推断变量类型(大写开头的类型名)。
fn resolve_var_type(content: &str, var: &str) -> Option<String> {
    let re =
        regex::Regex::new(&format!(r"\b([A-Z]\w*)(?:<[^>]*>)?\s+{}\b", regex::escape(var))).ok()?;
    content
        .lines()
        .find_map(|l| re.captures(l).map(|c| c[1].to_string()))
}

/// 解析类型 FQN:扫 import 命中类名;没有则视为同包(当前文件 package + 类型名)。
fn resolve_fqn(content: &str, type_name: &str) -> Option<String> {
    let import_re = regex::Regex::new(r"^import\s+(?:static\s+)?([\w.]+)\s*;").ok()?;
    let mut package: Option<String> = None;
    for l in content.lines() {
        let lt = l.trim();
        if package.is_none() && lt.starts_with("package ") && lt.ends_with(';') {
            package = Some(lt["package ".len()..lt.len() - 1].trim().to_string());
        }
        if let Some(c) = import_re.captures(lt) {
            if c[1].rsplit('.').next() == Some(type_name) {
                return Some(c[1].to_string());
            }
        }
    }
    package.map(|p| format!("{}.{}", p, type_name))
}

/// 在目标文件里 grep 方法声明行(优先带返回类型的真声明),返回 1-based 行号。
fn find_method_line(file_path: &str, method: &str) -> Option<usize> {
    let content = std::fs::read_to_string(file_path).ok()?;
    let decl = regex::Regex::new(&format!(
        r"\b\w+(?:<[^>]*>)?(?:\[\])?\s+{}\s*\(",
        regex::escape(method)
    ))
    .ok()?;
    let sig = regex::Regex::new(&format!(r"\b{}\s*\(", regex::escape(method))).ok()?;
    let modifiers = [
        "public",
        "protected",
        "private",
        "static",
        "abstract",
        "default",
        "synchronized",
        "final",
        "native",
    ];
    let lines: Vec<&str> = content.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let trimmed = l.trim_start();
        if modifiers.iter().any(|m| trimmed.starts_with(m)) && decl.is_match(l) {
            return Some(i + 1);
        }
    }
    lines.iter().position(|l| sig.is_match(l)).map(|i| i + 1)
}

/// 同文件内找方法定义(0-based 行),排除调用行。
fn find_method_def_in_content(content: &str, method: &str, exclude_line: usize) -> Option<u32> {
    let decl = regex::Regex::new(&format!(
        r"\b\w+(?:<[^>]*>)?(?:\[\])?\s+{}\s*\(",
        regex::escape(method)
    ))
    .ok()?;
    let modifiers = [
        "public",
        "protected",
        "private",
        "static",
        "abstract",
        "default",
        "synchronized",
        "final",
        "native",
    ];
    content.lines().enumerate().find_map(|(i, l)| {
        if i != exclude_line
            && modifiers.iter().any(|m| l.trim_start().starts_with(m))
            && decl.is_match(l)
        {
            Some(i as u32)
        } else {
            None
        }
    })
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

fn send_request(
    server: &LspServer,
    id: i64,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    send_message(server, &msg)
}

fn send_notification(
    server: &LspServer,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
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

    let mut writer = server.stdin.lock().map_err(|e| e.to_string())?;
    writer
        .write_all(header.as_bytes())
        .map_err(|e| format!("Failed to write to LSP stdin: {}", e))?;
    writer
        .write_all(body.as_bytes())
        .map_err(|e| format!("Failed to write to LSP stdin: {}", e))?;
    writer
        .flush()
        .map_err(|e| format!("Failed to flush LSP stdin: {}", e))?;
    Ok(())
}

fn read_next_message(reader: &mut BufReader<ChildStdout>) -> Result<serde_json::Value, String> {
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read LSP header: {}", e))?;
        if bytes_read == 0 {
            return Err("LSP stdout closed".into());
        }
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
        return Err("Missing Content-Length header".into());
    }
    // 64MB 封顶:畸形头(如 Content-Length: 999999999999)不许把进程 OOM 掉
    if content_length > 64 * 1024 * 1024 {
        return Err(format!("LSP message too large: {} bytes", content_length));
    }

    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("Failed to read LSP body: {}", e))?;

    serde_json::from_slice(&body).map_err(|e| format!("Invalid JSON from LSP: {}", e))
}

fn request_and_wait(
    server: &LspServer,
    id: i64,
    method: &str,
    params: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let rx = server.response_rx.lock().map_err(|e| e.to_string())?;

    // Drain stale responses left by previous timed-out requests
    while rx.try_recv().is_ok() {}

    send_request(server, id, method, params)?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "LSP {} timed out, please try again in a moment",
                method
            ));
        }

        let response = rx
            .recv_timeout(remaining)
            .map_err(|_| format!("LSP {} timed out, please try again in a moment", method))?;

        let resp_id = response.get("id").and_then(|v| v.as_i64()).unwrap_or(-1);
        if resp_id != id {
            continue; // skip stale response from a previous request
        }

        if let Some(error) = response.get("error") {
            return Err(format!("LSP error: {}", error));
        }

        return Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }
}

async fn request_and_wait_on_worker(
    server: Arc<LspServer>,
    id: i64,
    method: &'static str,
    params: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    crate::rt::spawn_blocking(move || {
        request_and_wait(&server, id, method, params, timeout)
    })
    .await
    .map_err(|e| format!("LSP worker failed: {}", e))?
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
        let character = start.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

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

#[derive(Serialize)]
pub struct DecompiledClass {
    pub path: String,
    pub content: String,
}

pub async fn find_class_in_maven(
    fqn: String,
    decompiled_dir: PathBuf,
) -> Result<Option<DecompiledClass>, String> {
    crate::rt::spawn_blocking(move || find_class_in_maven_blocking(&fqn, &decompiled_dir))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn find_class_in_maven_blocking(
    fqn: &str,
    decompiled_dir: &std::path::Path,
) -> Result<Option<DecompiledClass>, String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let m2_repo = format!("{}/.m2/repository", home);
    let class_path = fqn.replace('.', "/") + ".class";

    let source_jar = find_source_jar(&m2_repo, &class_path);
    if let Some(jar_path) = &source_jar {
        let java_path = fqn.replace('.', "/") + ".java";
        if let Ok(content) = extract_from_jar(jar_path, &java_path) {
            let cache_dir = decompiled_dir.to_string_lossy().to_string();
            let _ = std::fs::create_dir_all(&cache_dir);
            let file_name = fqn.split('.').next_back().unwrap_or("Unknown");
            let cache_path = format!("{}/{}.java", cache_dir, file_name);
            let _ = std::fs::write(&cache_path, &content);
            return Ok(Some(DecompiledClass {
                path: cache_path,
                content,
            }));
        }
    }

    let class_jar = find_class_jar(&m2_repo, &class_path);
    if let Some(jar_path) = class_jar {
        if let Ok(content) = decompile_class(&jar_path, fqn) {
            let cache_dir = decompiled_dir.to_string_lossy().to_string();
            let _ = std::fs::create_dir_all(&cache_dir);
            let file_name = fqn.split('.').next_back().unwrap_or("Unknown");
            let cache_path = format!("{}/{}.java", cache_dir, file_name);
            let _ = std::fs::write(&cache_path, &content);
            return Ok(Some(DecompiledClass {
                path: cache_path,
                content,
            }));
        }
    }

    if let Some(content) = find_class_in_jdk_src(fqn) {
        let cache_dir = decompiled_dir.to_string_lossy().to_string();
        let _ = std::fs::create_dir_all(&cache_dir);
        let file_name = fqn.split('.').next_back().unwrap_or("Unknown");
        let cache_path = format!("{}/{}.java", cache_dir, file_name);
        let _ = std::fs::write(&cache_path, &content);
        return Ok(Some(DecompiledClass {
            path: cache_path,
            content,
        }));
    }

    Ok(None)
}

fn find_java_home() -> Option<String> {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        if !home.is_empty() {
            return Some(home);
        }
    }
    let output = Command::new("/usr/libexec/java_home").output().ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() { Some(s) } else { None }
    } else {
        None
    }
}

fn find_class_in_jdk_src(fqn: &str) -> Option<String> {
    let java_home = find_java_home()?;
    let src_zip = format!("{}/lib/src.zip", java_home);
    if !std::path::Path::new(&src_zip).exists() {
        return None;
    }
    let java_path = fqn.replace('.', "/") + ".java";
    let pattern = format!("*/{}", java_path);
    let output = Command::new("unzip")
        .args(["-p", &src_zip, &pattern])
        .output()
        .ok()?;
    if output.status.success() && !output.stdout.is_empty() {
        return Some(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let output = Command::new("unzip")
        .args(["-p", &src_zip, &java_path])
        .output()
        .ok()?;
    if output.status.success() && !output.stdout.is_empty() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

fn find_source_jar(m2_repo: &str, class_path: &str) -> Option<String> {
    let parts: Vec<&str> = class_path.rsplitn(2, '/').collect();
    if parts.len() < 2 {
        return None;
    }
    let package_dir = parts[1];
    let package_parts: Vec<&str> = package_dir.split('/').collect();

    for depth in (2..=package_parts.len().min(5)).rev() {
        let group_path = package_parts[..depth].join("/");
        let search_dir = format!("{}/{}", m2_repo, group_path);
        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let artifact_dir = entry.path();
                if !artifact_dir.is_dir() {
                    continue;
                }
                if let Ok(versions) = std::fs::read_dir(&artifact_dir) {
                    for ver in versions.flatten() {
                        let ver_dir = ver.path();
                        if !ver_dir.is_dir() {
                            continue;
                        }
                        if let Ok(files) = std::fs::read_dir(&ver_dir) {
                            for file in files.flatten() {
                                let name = file.file_name().to_string_lossy().to_string();
                                if name.ends_with("-sources.jar") {
                                    let jar_path = file.path().to_string_lossy().to_string();
                                    let java_path = class_path.replace(".class", ".java");
                                    if jar_contains(&jar_path, &java_path) {
                                        return Some(jar_path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_class_jar(m2_repo: &str, class_path: &str) -> Option<String> {
    let parts: Vec<&str> = class_path.rsplitn(2, '/').collect();
    if parts.len() < 2 {
        return None;
    }
    let package_dir = parts[1];
    let package_parts: Vec<&str> = package_dir.split('/').collect();

    for depth in (2..=package_parts.len().min(5)).rev() {
        let group_path = package_parts[..depth].join("/");
        let search_dir = format!("{}/{}", m2_repo, group_path);
        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let artifact_dir = entry.path();
                if !artifact_dir.is_dir() {
                    continue;
                }
                if let Ok(versions) = std::fs::read_dir(&artifact_dir) {
                    for ver in versions.flatten() {
                        let ver_dir = ver.path();
                        if !ver_dir.is_dir() {
                            continue;
                        }
                        if let Ok(files) = std::fs::read_dir(&ver_dir) {
                            for file in files.flatten() {
                                let name = file.file_name().to_string_lossy().to_string();
                                if name.ends_with(".jar")
                                    && !name.ends_with("-sources.jar")
                                    && !name.ends_with("-javadoc.jar")
                                {
                                    let jar_path = file.path().to_string_lossy().to_string();
                                    if jar_contains(&jar_path, class_path) {
                                        return Some(jar_path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn jar_contains(jar_path: &str, entry_path: &str) -> bool {
    let output = Command::new("jar").args(["tf", jar_path]).output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().any(|l| l == entry_path)
        }
        Err(_) => false,
    }
}

fn extract_from_jar(jar_path: &str, entry_path: &str) -> Result<String, String> {
    let output = Command::new("unzip")
        .args(["-p", jar_path, entry_path])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err("Failed to extract from jar".into())
    }
}

fn decompile_class(jar_path: &str, fqn: &str) -> Result<String, String> {
    let output = Command::new("javap")
        .args(["-p", "-c", "-cp", jar_path, fqn])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 验证 kill_all 真正杀死并回收子进程(应用退出路径 RunEvent::Exit 依赖它,
    // 否则 jdtls 孤儿跨重启累积——曾在真机上发现存活 23 小时的孤儿实例)。
    #[test]
    fn kill_all_reaps_server_processes() {
        let mut child = Command::new("cat") // 读 stdin 永不退出,模拟常驻 LSP
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cat");
        let pid = child.id();
        let stdin = child.stdin.take().unwrap();

        let (_tx, rx) = mpsc::channel::<serde_json::Value>();
        let server = LspServer {
            stdin: Arc::new(Mutex::new(std::io::BufWriter::new(stdin))),
            response_rx: Mutex::new(rx),
            next_id: AtomicI64::new(1),
            root_uri: "file:///tmp".into(),
            process: Mutex::new(child),
            ready: Arc::new(AtomicBool::new(true)),
        };
        let state = LspState::default();
        state
            .servers
            .lock()
            .unwrap()
            .insert(("test".into(), "/tmp".into()), Arc::new(server));

        state.kill_all();

        // kill(pid, 0) 失败(ESRCH)= 进程已不存在且已被回收(wait 过,非僵尸)
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        assert!(!alive, "kill_all 后子进程 {} 仍存活", pid);
        assert!(state.servers.lock().unwrap().is_empty(), "服务表应已清空");
    }
}
