use md5::{Digest, Md5};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::State;

// ── Constants ──

const PROD_LOGIN_URL: &str = "https://login.alibaba-inc.com/authorize/login.do";
const PROD_ACCESS_URL: &str = "https://login.alibaba-inc.com/authorize/getAccessToken.do";
const PROD_VALID_URL: &str = "https://login.alibaba-inc.com/authorize/validateAccessToken.do";
const PROD_REFRESH_URL: &str = "https://login.alibaba-inc.com/authorize/renewAccessToken.do";
const PROD_SSO_URL: &str = "https://login.alibaba-inc.com/authorize/getSSOToken.do";
const PROD_APP_CODE: &str = "b79494d528054d7788b7bdca96045594";
const PROD_SALT: &str = "ec0a1b1d-39de-41c0-a500-43dfd9654cf2";

const DAILY_LOGIN_URL: &str = "https://login-test.alibaba-inc.com/authorize/login.do";
const DAILY_ACCESS_URL: &str = "https://login-test.alibaba-inc.com/authorize/getAccessToken.do";
const DAILY_VALID_URL: &str = "https://login-test.alibaba-inc.com/authorize/validateAccessToken.do";
const DAILY_REFRESH_URL: &str = "https://login-test.alibaba-inc.com/authorize/renewAccessToken.do";
const DAILY_SSO_URL: &str = "https://login-test.alibaba-inc.com/authorize/getSSOToken.do";
const DAILY_APP_CODE: &str = "fd538e1647fb4e5c853ae7a11c98f056";
const DAILY_SALT: &str = "9ae0034b-e847-4c1d-b401-79cd4227ba9c";

// ── Types ──

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AstoreSession {
    pub username: String,
    pub nick_name: String,
    pub emp_id: String,
    pub auth_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub sso_token: String,
    pub current_env: String, // "日常环境" or "预发环境"
    pub template_id: Option<i64>,
    pub site_id: Option<i64>,
    pub change_record_id: Option<i64>,
    pub tenant_code: Option<String>,
    pub biz_code: Option<String>,
    pub project_type: String, // "astore", "inside", or "none"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstoreResult<T> {
    pub success: Option<bool>,
    pub message: Option<String>,
    pub data: Option<T>,
    pub code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub id: Option<i64>,
    pub entity_id: Option<i64>,
    pub entity_name: Option<String>,
    pub site_id: Option<i64>,
    pub biz_code: Option<String>,
    pub tenant_code: Option<String>,
    pub app_name: Option<String>,
    pub status: Option<String>,
    pub latest_daily_publish_id: Option<i64>,
    pub latest_pre_publish_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishInfoPayload {
    pub entity_type: String,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<HashMap<String, serde_json::Value>>,
}

pub struct AstoreState {
    pub session: Mutex<AstoreSession>,
    pub password: Mutex<Option<String>>,
    client: Client,
}

impl Default for AstoreState {
    fn default() -> Self {
        let client = Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(50))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());
        let session = load_session_from_disk().unwrap_or_default();
        Self {
            session: Mutex::new(session),
            password: Mutex::new(None),
            client,
        }
    }
}

// ── Persistence ──

fn session_file_path() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("lite-editor").join("astore-session.json")
}

fn load_session_from_disk() -> Option<AstoreSession> {
    let path = session_file_path();
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_session_to_disk(session: &AstoreSession) {
    let path = session_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(session).unwrap_or_default());
}

// ── Helpers ──

fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode_upper(hasher.finalize())
}

fn millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn build_url(is_inside: bool, path: &str, params: &[(&str, &str)]) -> String {
    let host = if is_inside {
        "pre-astore-inside.alibaba-inc.com"
    } else {
        "astore.alibaba-inc.com"
    };
    let mut url = format!("https://{}{}", host, path);
    if !params.is_empty() {
        url.push('?');
        let qs: Vec<String> = params.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        url.push_str(&qs.join("&"));
    }
    url
}

fn append_sso(url: &str, sso: &str) -> String {
    if url.contains('?') {
        format!("{}&SSO_TICKET={}", url, sso)
    } else {
        format!("{}?SSO_TICKET={}", url, sso)
    }
}

// ── Auth flow ──

async fn do_login(
    client: &Client,
    username: &str,
    password: &str,
) -> Result<AstoreSession, String> {
    let time = millis();
    let sign_input = format!(
        "{}appcode={}&name={}&password={}&time={}",
        PROD_SALT, PROD_APP_CODE, username, password, time
    );
    let sign = md5_hex(&sign_input);

    let params = [
        ("appcode", PROD_APP_CODE),
        ("name", username),
        ("password", password),
        ("time", &time.to_string()),
        ("sign", &sign),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(PROD_LOGIN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("登录请求失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("登录响应解析失败: {}", e))?;

    if resp.get("IsSuccess").and_then(|v| v.as_str()) != Some("true") {
        let code = resp
            .get("ErrorCode")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("登录失败，错误码: {}", code));
    }

    let auth_code = resp
        .get("ArkAuthCode")
        .and_then(|v| v.as_str())
        .ok_or("登录成功但未获取到 ArkAuthCode")?
        .to_string();

    let access_token = get_access_token(client, &auth_code, false).await?;
    let (nick, emp_id, refresh_token) = validate_token(client, &access_token, false).await?;
    let (access_token, refresh_token) =
        refresh_access_token(client, &access_token, &refresh_token, false).await?;
    let sso_token = get_sso_token(client, &access_token, false).await?;

    Ok(AstoreSession {
        username: username.to_string(),
        nick_name: nick,
        emp_id,
        auth_token: auth_code,
        access_token,
        refresh_token,
        sso_token,
        current_env: "日常环境".to_string(),
        project_type: "none".to_string(),
        ..Default::default()
    })
}

async fn get_access_token(
    client: &Client,
    auth_code: &str,
    daily: bool,
) -> Result<String, String> {
    let (url, app_code, salt) = if daily {
        (DAILY_ACCESS_URL, DAILY_APP_CODE, DAILY_SALT)
    } else {
        (PROD_ACCESS_URL, PROD_APP_CODE, PROD_SALT)
    };
    let time = millis();
    let sign_input = format!(
        "{}appcode={}&authcode={}&time={}",
        salt, app_code, auth_code, time
    );
    let sign = md5_hex(&sign_input);

    let params = [
        ("appcode", app_code),
        ("authcode", auth_code),
        ("time", &time.to_string()),
        ("sign", &sign),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("获取 accessToken 失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("accessToken 响应解析失败: {}", e))?;

    if resp.get("IsSuccess").and_then(|v| v.as_str()) == Some("true") {
        resp.get("AccessToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or("响应中无 AccessToken".to_string())
    } else {
        Err("获取 accessToken 失败".to_string())
    }
}

async fn validate_token(
    client: &Client,
    access_token: &str,
    daily: bool,
) -> Result<(String, String, String), String> {
    let (url, app_code, salt) = if daily {
        (DAILY_VALID_URL, DAILY_APP_CODE, DAILY_SALT)
    } else {
        (PROD_VALID_URL, PROD_APP_CODE, PROD_SALT)
    };
    let time = millis();
    let sign_input = format!(
        "{}appcode={}&accesstoken={}&time={}",
        salt, app_code, access_token, time
    );
    let sign = md5_hex(&sign_input);

    let params = [
        ("type", "tmn"),
        ("appcode", app_code),
        ("accesstoken", access_token),
        ("sign", &sign),
        ("time", &time.to_string()),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("验证 token 失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("验证 token 响应解析失败: {}", e))?;

    if resp.get("ErrorCode").and_then(|v| v.as_str()) == Some("0") {
        let nick = resp
            .get("nickName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let emp_id = resp
            .get("WorkId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let refresh = resp
            .get("RefreshToken")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok((nick, emp_id, refresh))
    } else {
        Err("验证 token 失败".to_string())
    }
}

async fn refresh_access_token(
    client: &Client,
    access_token: &str,
    refresh_token: &str,
    daily: bool,
) -> Result<(String, String), String> {
    let url = if daily {
        DAILY_REFRESH_URL
    } else {
        PROD_REFRESH_URL
    };

    let params = [
        ("accesstoken", access_token),
        ("refreshtoken", refresh_token),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("刷新 token 失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("刷新 token 响应解析失败: {}", e))?;

    if resp.get("IsSuccess").and_then(|v| v.as_str()) == Some("true") {
        let at = resp
            .get("accessToken")
            .and_then(|v| v.as_str())
            .unwrap_or(access_token)
            .to_string();
        let rt = resp
            .get("refreshToken")
            .and_then(|v| v.as_str())
            .unwrap_or(refresh_token)
            .to_string();
        Ok((at, rt))
    } else {
        Err("刷新 token 失败".to_string())
    }
}

async fn get_sso_token(client: &Client, access_token: &str, daily: bool) -> Result<String, String> {
    let (url, app_code, salt) = if daily {
        (DAILY_SSO_URL, DAILY_APP_CODE, DAILY_SALT)
    } else {
        (PROD_SSO_URL, PROD_APP_CODE, PROD_SALT)
    };
    let time = millis();
    let sign_input = format!(
        "{}appcode={}&accesstoken={}&time={}",
        salt, app_code, access_token, time
    );
    let sign = md5_hex(&sign_input);

    let params = [
        ("accesstoken", access_token),
        ("time", &time.to_string()),
        ("appcode", app_code),
        ("sign", &sign),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("获取 SSO token 失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("SSO token 响应解析失败: {}", e))?;

    if resp.get("success").and_then(|v| v.as_str()) == Some("true") {
        resp.get("ssoToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or("响应中无 ssoToken".to_string())
    } else {
        Err("获取 SSO token 失败".to_string())
    }
}

async fn get_daily_sso(
    client: &Client,
    username: &str,
    password: &str,
) -> Result<String, String> {
    let time = millis();
    let sign_input = format!(
        "{}appcode={}&name={}&password={}&time={}",
        DAILY_SALT, PROD_APP_CODE, username, password, time
    );
    let sign = md5_hex(&sign_input);

    let params = [
        ("appcode", DAILY_APP_CODE),
        ("name", username),
        ("password", password),
        ("time", &time.to_string()),
        ("sign", &sign),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(DAILY_LOGIN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("日常登录失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("日常登录响应解析失败: {}", e))?;

    if resp.get("IsSuccess").and_then(|v| v.as_str()) != Some("true") {
        return Err("日常登录失败".to_string());
    }

    let auth_code = resp
        .get("ArkAuthCode")
        .and_then(|v| v.as_str())
        .ok_or("日常登录无 ArkAuthCode")?
        .to_string();

    let access_token = get_access_token(client, &auth_code, true).await?;
    let (_, _, refresh_token) = validate_token(client, &access_token, true).await?;
    let (access_token, _) =
        refresh_access_token(client, &access_token, &refresh_token, true).await?;
    get_sso_token(client, &access_token, true).await
}

#[allow(dead_code)] // 预留:SSO 过期后的刷新路径,尚未接线
async fn refresh_sso(client: &Client, session: &mut AstoreSession) -> Result<(), String> {
    let time = millis();
    let params = [
        ("appcode", PROD_APP_CODE.to_string()),
        ("accesstoken", session.access_token.clone()),
        ("time", time.to_string()),
    ];

    let resp: HashMap<String, serde_json::Value> = client
        .post(PROD_SSO_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("刷新 SSO 失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("刷新 SSO 响应解析失败: {}", e))?;

    if resp.get("success").and_then(|v| v.as_str()) == Some("true") {
        if let Some(token) = resp.get("ssoToken").and_then(|v| v.as_str()) {
            session.sso_token = token.to_string();
            return Ok(());
        }
    }

    Err("SSO token 已过期，请重新登录".to_string())
}

// ── API calls ──

async fn api_get(
    client: &Client,
    url: &str,
    sso: &str,
    tenant_header: Option<&str>,
) -> Result<AstoreResult<serde_json::Value>, String> {
    let full_url = append_sso(url, sso);
    let mut req = client.get(&full_url);
    if let Some(tenant) = tenant_header {
        req = req.header("x-astore-tenant", tenant);
    }
    req.send()
        .await
        .map_err(|e| format!("API 请求失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("API 响应解析失败: {}", e))
}

async fn api_post_json(
    client: &Client,
    url: &str,
    sso: &str,
    body: &str,
    tenant_header: Option<&str>,
) -> Result<AstoreResult<serde_json::Value>, String> {
    let full_url = append_sso(url, sso);
    let mut req = client
        .post(&full_url)
        .header("Content-Type", "application/json;charset=UTF-8")
        .body(body.to_string());
    if let Some(tenant) = tenant_header {
        req = req.header("x-astore-tenant", tenant);
    }
    req.send()
        .await
        .map_err(|e| format!("API POST 失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("API POST 响应解析失败: {}", e))
}

// ── Project detection ──

fn detect_project(dir_name: &str) -> (String, Option<i64>) {
    // astore: template_xxx or Template_xxx
    if dir_name.starts_with("template_") || dir_name.starts_with("Template_") {
        if let Some(id) = dir_name.rsplit('_').next().and_then(|s| s.parse::<i64>().ok()) {
            if id > 0 {
                return ("astore".to_string(), Some(id));
            }
        }
    }
    // inside: page-xxx, pages-xxx, template-xxx, page_xxx
    for prefix in &["page-", "pages-", "template-", "page_", "pages_"] {
        if dir_name.starts_with(prefix) {
            // try dash split first
            if let Some(id) = dir_name.rsplit('-').next().and_then(|s| s.parse::<i64>().ok()) {
                if id > 0 {
                    return ("inside".to_string(), Some(id));
                }
            }
            if let Some(id) = dir_name.rsplit('_').next().and_then(|s| s.parse::<i64>().ok()) {
                if id > 0 {
                    return ("inside".to_string(), Some(id));
                }
            }
        }
    }
    ("none".to_string(), None)
}

// ── Tauri Commands ──

#[tauri::command]
pub async fn astore_login(
    state: State<'_, AstoreState>,
    username: String,
    password: String,
) -> Result<AstoreSession, String> {
    let mut session = do_login(&state.client, &username, &password).await?;

    // Keep password in memory for token refresh
    *state.password.lock().unwrap() = Some(password);

    // Preserve project-specific fields if already detected
    let old = state.session.lock().unwrap().clone();
    if old.template_id.is_some() {
        session.template_id = old.template_id;
        session.project_type = old.project_type;
    }
    if !old.current_env.is_empty() {
        session.current_env = old.current_env;
    }

    save_session_to_disk(&session);
    *state.session.lock().unwrap() = session.clone();
    Ok(session)
}

#[tauri::command]
pub async fn astore_logout(state: State<'_, AstoreState>) -> Result<(), String> {
    let mut session = state.session.lock().unwrap();
    session.username.clear();
    session.nick_name.clear();
    session.emp_id.clear();
    session.auth_token.clear();
    session.access_token.clear();
    session.refresh_token.clear();
    session.sso_token.clear();
    save_session_to_disk(&session);
    *state.password.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
pub async fn astore_get_session(state: State<'_, AstoreState>) -> Result<AstoreSession, String> {
    Ok(state.session.lock().unwrap().clone())
}

#[tauri::command]
pub async fn astore_detect_project(
    state: State<'_, AstoreState>,
    project_path: String,
) -> Result<AstoreSession, String> {
    let dir_name = std::path::Path::new(&project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let (ptype, template_id) = detect_project(dir_name);

    let mut session = state.session.lock().unwrap();
    session.project_type = ptype;
    session.template_id = template_id;
    save_session_to_disk(&session);
    Ok(session.clone())
}

#[tauri::command]
pub async fn astore_set_env(
    state: State<'_, AstoreState>,
    env: String,
) -> Result<(), String> {
    let mut session = state.session.lock().unwrap();
    session.current_env = env;
    save_session_to_disk(&session);
    Ok(())
}

// git remote URL 规范化(自前端 astore-panel.ts 迁入):scp 形态 git@host:path →
// http://host/path;http(s) 等其余形态原样保留(原 TS 实现对已是 http 的 URL
// 也替换首个冒号,会把 "http://x" 破坏成 "http///x")。
fn normalize_git_url(url: &str) -> String {
    let url = url.trim();
    match url.strip_prefix("git@") {
        Some(rest) => format!("http://{}", rest.replacen(':', "/", 1)),
        None => url.to_string(),
    }
}

#[tauri::command]
pub async fn astore_sync_code(
    state: State<'_, AstoreState>,
    git_url: String,
    branch: String,
) -> Result<String, String> {
    let git_url = normalize_git_url(&git_url);
    let session = state.session.lock().unwrap().clone();
    if session.sso_token.is_empty() {
        return Err("未登录".to_string());
    }
    let template_id = session.template_id.ok_or("未检测到模板 ID")?;

    if session.project_type == "inside" {
        let url = format!(
            "https://pre-astore-inside.alibaba-inc.com/editor/saveProjectScript.do?pageId={}&branch={}",
            template_id, branch
        );
        let result: AstoreResult<serde_json::Value> = state
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| format!("同步失败: {}", e))?
            .json()
            .await
            .map_err(|e| format!("同步响应解析失败: {}", e))?;
        if result.success == Some(true) {
            Ok("同步代码到 Astore 完成".to_string())
        } else {
            Err(result.message.unwrap_or("同步失败".to_string()))
        }
    } else {
        let url = format!(
            "https://astore.alibaba-inc.com/editor/saveTemplateScript?gitUrl={}",
            git_url
        );
        let result = api_get(&state.client, &url, &session.sso_token, None).await?;
        if result.success == Some(true) {
            Ok("同步代码到 Astore 完成".to_string())
        } else {
            Err(result.message.unwrap_or("同步失败".to_string()))
        }
    }
}

#[tauri::command]
pub async fn astore_publish(
    state: State<'_, AstoreState>,
) -> Result<String, String> {
    let session = state.session.lock().unwrap().clone();
    if session.sso_token.is_empty() {
        return Err("未登录".to_string());
    }
    let template_id = session.template_id.ok_or("未检测到模板 ID")?;
    let is_inside = session.project_type == "inside";
    let is_daily = session.current_env == "日常环境";

    // Step 1: Create/get change record
    let record = get_change_record(&state, template_id, is_inside).await?;
    let record_id = record.id.ok_or("发布单 ID 为空")?;

    // Update session with record info
    {
        let mut s = state.session.lock().unwrap();
        s.change_record_id = Some(record_id);
        s.site_id = record.site_id;
        s.tenant_code = record.tenant_code.clone();
        s.biz_code = record.biz_code.clone();
        save_session_to_disk(&s);
    }

    if is_daily {
        publish_daily(&state, &record, is_inside).await
    } else {
        publish_pre(&state, &record, is_inside).await
    }
}

async fn get_change_record(
    state: &State<'_, AstoreState>,
    template_id: i64,
    is_inside: bool,
) -> Result<ChangeRecord, String> {
    let session = state.session.lock().unwrap().clone();
    let tid_str = template_id.to_string();

    if is_inside {
        let url = build_url(
            true,
            "/changeRecord/createChangeRecord.do",
            &[
                ("entityId", &tid_str),
                ("entityType", "PAGE"),
            ],
        );
        let body = serde_json::json!({
            "_employee": {
                "empId": session.emp_id,
                "name": session.username,
                "nick": session.nick_name,
            },
            "entityId": template_id,
            "entityType": "PAGE",
        });
        let result: AstoreResult<ChangeRecord> = state
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| format!("创建发布单失败: {}", e))?
            .json()
            .await
            .map_err(|e| format!("发布单响应解析失败: {}", e))?;
        result.data.ok_or("发布单为空".to_string())
    } else {
        let url = build_url(
            false,
            "/changeRecord/createChangeRecord",
            &[
                ("entityId", &tid_str),
                ("entityType", "TEMPLATE"),
            ],
        );
        let full_url = append_sso(&url, &session.sso_token);
        let result: AstoreResult<ChangeRecord> = state
            .client
            .get(&full_url)
            .send()
            .await
            .map_err(|e| format!("创建发布单失败: {}", e))?
            .json()
            .await
            .map_err(|e| format!("发布单响应解析失败: {}", e))?;
        result.data.ok_or("发布单为空".to_string())
    }
}

async fn publish_daily(
    state: &State<'_, AstoreState>,
    record: &ChangeRecord,
    is_inside: bool,
) -> Result<String, String> {
    let session = state.session.lock().unwrap().clone();
    let record_id = record.id.unwrap().to_string();

    // Generate publish info
    let gen_url = if is_inside {
        
        build_url(
            true,
            "/changeRecord/generatePublishInfo.do",
            &[
                ("changeRecordId", &record_id),
                ("empId", &session.emp_id),
                ("nick", &session.nick_name),
                ("name", &session.username),
            ],
        )
    } else {
        let url = build_url(
            false,
            "/changeRecord/generatePublishInfo",
            &[("changeRecordId", &record_id)],
        );
        append_sso(&url, &session.sso_token)
    };

    let gen_result: AstoreResult<String> = state
        .client
        .get(&gen_url)
        .send()
        .await
        .map_err(|e| format!("生成发布信息失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("生成发布信息响应解析失败: {}", e))?;

    if gen_result.success != Some(true) {
        return Err(gen_result.message.unwrap_or("生成发布信息失败".to_string()));
    }

    let publish_data = gen_result.data.unwrap_or_default();

    // Publish daily
    let entity_type = if is_inside { "PAGE" } else { "TEMPLATE" };
    let mut payload = PublishInfoPayload {
        entity_type: entity_type.to_string(),
        data: publish_data,
        extensions: None,
    };

    if is_inside {
        let mut ext = HashMap::new();
        ext.insert(
            "_employee".to_string(),
            serde_json::json!({
                "empId": session.emp_id,
                "name": session.username,
                "nick": session.nick_name,
            }),
        );
        payload.extensions = Some(ext);
    }

    let publish_url = if is_inside {
        "https://astore-inside.alibaba.net/changeRecord/publishDaily.do"
    } else {
        "https://stable-astore.alibaba.net/changeRecord/publishDaily"
    };

    // For regular astore publishDaily, we need daily SSO
    let body = serde_json::to_string(&payload).map_err(|e| format!("序列化失败: {}", e))?;

    let publish_result = if is_inside {
        api_post_json(&state.client, publish_url, "", &body, None).await?
    } else {
        // Regular astore needs daily SSO token for publishDaily
        let password = state.password.lock().unwrap().clone();
        let password = password.ok_or("需要密码来获取日常 SSO token，请重新登录")?;
        let daily_sso = get_daily_sso(&state.client, &session.username, &password).await?;
        let url = format!("{}?SSO_TICKET={}", publish_url, daily_sso);
        state
            .client
            .post(&url)
            .header("Content-Type", "application/json;charset=UTF-8")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("发布日常失败: {}", e))?
            .json()
            .await
            .map_err(|e| format!("发布日常响应解析失败: {}", e))?
    };

    if publish_result.success != Some(true) {
        return Err(publish_result.message.unwrap_or("发布日常失败".to_string()));
    }

    // Sync daily
    let sync_url = if is_inside {
        
        build_url(
            true,
            "/changeRecord/syncDaily.do",
            &[("changeRecordId", &record_id)],
        )
    } else {
        let url = build_url(
            false,
            "/changeRecordV2/syncDaily",
            &[("changeRecordId", &record_id)],
        );
        append_sso(&url, &session.sso_token)
    };

    let sync_result: AstoreResult<serde_json::Value> = state
        .client
        .get(&sync_url)
        .send()
        .await
        .map_err(|e| format!("同步日常失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("同步日常响应解析失败: {}", e))?;

    if sync_result.success != Some(true) {
        return Err(sync_result.message.unwrap_or("同步日常失败".to_string()));
    }

    // Build result URL
    let session = state.session.lock().unwrap().clone();
    let url = get_record_url(&session);
    Ok(format!("发布日常成功！查看发布记录: {}", url))
}

async fn publish_pre(
    state: &State<'_, AstoreState>,
    record: &ChangeRecord,
    is_inside: bool,
) -> Result<String, String> {
    let session = state.session.lock().unwrap().clone();
    let record_id = record.id.unwrap().to_string();
    let tenant = record.tenant_code.as_deref().unwrap_or("");

    let sync_url = if is_inside {
        format!(
            "https://pre-astore-inside.alibaba-inc.com/changeRecord/syncPre.do?changeRecordId={}&empId={}",
            record_id, session.emp_id
        )
    } else {
        format!(
            "https://pre-stable-astore.alibaba-inc.com/changeRecordV2/syncPre?changeRecordId={}&empId={}&SSO_TICKET={}",
            record_id, session.emp_id, session.sso_token
        )
    };

    let result: AstoreResult<serde_json::Value> = state
        .client
        .get(&sync_url)
        .header("x-astore-tenant", tenant)
        .send()
        .await
        .map_err(|e| format!("发布预发失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("发布预发响应解析失败: {}", e))?;

    if result.success != Some(true) {
        return Err(result.message.unwrap_or("发布预发失败".to_string()));
    }

    let url = get_record_url(&session);
    Ok(format!("发布预发成功！查看发布记录: {}", url))
}

fn get_record_url(session: &AstoreSession) -> String {
    if session.project_type == "inside" {
        format!(
            "https://astore-inside.alibaba-inc.com/app/#/tenant/{}/site/{}/publishRecord/detail/{}",
            session.tenant_code.as_deref().unwrap_or(""),
            session.biz_code.as_deref().unwrap_or(""),
            session.change_record_id.unwrap_or(0)
        )
    } else {
        format!(
            "https://astore.alibaba-inc.com/#/site/{}/publishRecord/detail/{}",
            session.site_id.unwrap_or(0),
            session.change_record_id.unwrap_or(0)
        )
    }
}

#[tauri::command]
pub async fn astore_get_editor_url(
    state: State<'_, AstoreState>,
) -> Result<String, String> {
    let session = state.session.lock().unwrap().clone();
    let tid = session.template_id.ok_or("未检测到模板 ID")?;
    if session.project_type == "inside" {
        Ok(format!(
            "https://astore-inside.alibaba-inc.com/editor?templateId={}",
            tid
        ))
    } else {
        Ok(format!(
            "https://astore.alibaba-inc.com/editor?templateId={}",
            tid
        ))
    }
}

#[tauri::command]
pub async fn astore_get_record_url(
    state: State<'_, AstoreState>,
) -> Result<String, String> {
    let session = state.session.lock().unwrap().clone();
    Ok(get_record_url(&session))
}

#[cfg(test)]
mod tests {
    use super::normalize_git_url;

    #[test]
    fn normalizes_scp_form_to_http() {
        assert_eq!(
            normalize_git_url("git@gitlab.alibaba-inc.com:group/repo.git"),
            "http://gitlab.alibaba-inc.com/group/repo.git"
        );
    }

    #[test]
    fn keeps_http_form_and_trims_whitespace() {
        assert_eq!(
            normalize_git_url("  http://gitlab.alibaba-inc.com/group/repo.git\n"),
            "http://gitlab.alibaba-inc.com/group/repo.git"
        );
        assert_eq!(
            normalize_git_url("https://github.com/a/b.git"),
            "https://github.com/a/b.git"
        );
    }
}
