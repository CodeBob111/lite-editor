import {
  astoreLogin,
  astoreLogout,
  astoreGetSession,
  astoreDetectProject,
  astoreSetEnv,
  astoreSyncCode,
  astorePublish,
  astoreGetEditorUrl,
  gitRemoteUrl,
  gitCurrentBranch,
  type AstoreSession,
} from "./tauri-api";

let panelEl: HTMLElement;
let messageEl: HTMLElement;
let session: AstoreSession | null = null;
let currentProjectPath = "";

export function initAstorePanel(
  container: HTMLElement,
  msgContainer: HTMLElement,
) {
  panelEl = container;
  messageEl = msgContainer;
  render();
  loadSession();
}

export function toggleAstorePanel() {
  const appEl = document.getElementById("app");
  if (!appEl) return;
  appEl.classList.toggle("astore-visible");
  if (!appEl.classList.contains("astore-visible")) leaveAstoreMsgTabIfActive();
}

export function showAstorePanel() {
  const appEl = document.getElementById("app");
  if (appEl) appEl.classList.add("astore-visible");
}

export function hideAstorePanel() {
  const appEl = document.getElementById("app");
  if (appEl) appEl.classList.remove("astore-visible");
  leaveAstoreMsgTabIfActive();
}

// Astore Message tab 会随 astore-visible 一起隐藏；若当前正停在该面板上，
// 切回 Terminal,避免留下「已激活但不可见」的空面板(Maven 已移到左侧栏)。
function leaveAstoreMsgTabIfActive() {
  if (document.getElementById("astore-msg-panel")?.classList.contains("active")) {
    document.querySelector<HTMLElement>('.panel-tab[data-panel="terminal"]')?.click();
  }
}

export async function onProjectChanged(projectPath: string) {
  currentProjectPath = projectPath;
  if (!projectPath) return;
  try {
    session = await astoreDetectProject(projectPath);
    render();
    if (session.projectType !== "none") {
      showAstorePanel();
      appendMessage(
        "info",
        `检测到 Astore 项目: ${session.projectType} (templateId=${session.templateId})`,
      );
    } else {
      hideAstorePanel();
    }
  } catch {
    hideAstorePanel();
  }
}

async function loadSession() {
  try {
    session = await astoreGetSession();
    render();
  } catch {
    // no session
  }
}

function render() {
  if (!panelEl) return;
  const loggedIn = session && session.accessToken;
  const isAstore = session && session.projectType !== "none";

  panelEl.innerHTML = "";

  // Env selector
  if (loggedIn && isAstore) {
    const envRow = el("div", "astore-env-row");
    const select = document.createElement("select");
    select.className = "astore-env-select";
    const daily = document.createElement("option");
    daily.value = "日常环境";
    daily.textContent = "日常环境";
    const pre = document.createElement("option");
    pre.value = "预发环境";
    pre.textContent = "预发环境";
    select.append(daily, pre);
    select.value = session!.currentEnv || "日常环境";
    select.onchange = async () => {
      await astoreSetEnv(select.value);
      session!.currentEnv = select.value;
    };
    envRow.append(select);
    panelEl.append(envRow);
  }

  // Login / Logout
  if (loggedIn) {
    const logoutRow = el("div", "astore-logout-row");
    const userSpan = el("span", "astore-user");
    userSpan.textContent = `用户: ${session!.nickName || session!.username}`;
    const logoutBtn = document.createElement("button");
    logoutBtn.className = "astore-btn astore-btn-small";
    logoutBtn.textContent = "登出";
    logoutBtn.onclick = handleLogout;
    logoutRow.append(userSpan, logoutBtn);
    panelEl.append(logoutRow);
  } else {
    const loginForm = el("div", "astore-login-form");
    const uRow = el("div", "astore-form-row");
    const uLabel = el("label", "astore-label");
    uLabel.textContent = "账号:";
    const uInput = document.createElement("input");
    uInput.type = "text";
    uInput.className = "astore-input";
    uInput.placeholder = "域账号";
    uInput.id = "astore-username";
    uRow.append(uLabel, uInput);

    const pRow = el("div", "astore-form-row");
    const pLabel = el("label", "astore-label");
    pLabel.textContent = "密码:";
    const pInput = document.createElement("input");
    pInput.type = "password";
    pInput.className = "astore-input";
    pInput.placeholder = "域密码";
    pInput.id = "astore-password";
    pInput.onkeydown = (e) => {
      if (e.key === "Enter") handleLogin();
    };
    pRow.append(pLabel, pInput);

    const loginBtn = document.createElement("button");
    loginBtn.className = "astore-btn";
    loginBtn.textContent = "登录";
    loginBtn.onclick = handleLogin;

    loginForm.append(uRow, pRow, loginBtn);
    panelEl.append(loginForm);
  }

  // Not astore project warning
  if (!isAstore) {
    const notice = el("div", "astore-notice");
    notice.textContent =
      loggedIn
        ? "当前项目不是 Astore 模板"
        : "请先登录 Astore";
    panelEl.append(notice);
    return;
  }

  // Status
  const statusRow = el("div", "astore-status-row");
  const statusLabel = el("span", "astore-status-label");
  statusLabel.textContent = "模板状态:";
  const statusVal = el("span", "astore-status-val");
  statusVal.textContent = "待发布";
  statusVal.id = "astore-status-text";
  statusRow.append(statusLabel, statusVal);
  panelEl.append(statusRow);

  // Template info
  const infoRow = el("div", "astore-info-row");
  infoRow.textContent = `Template ID: ${session!.templateId} (${session!.projectType})`;
  panelEl.append(infoRow);

  // Separator
  panelEl.append(el("hr", "astore-sep"));

  // Actions
  const actionsRow = el("div", "astore-actions");

  const syncBtn = document.createElement("button");
  syncBtn.className = "astore-btn";
  syncBtn.textContent = "同步";
  syncBtn.title = "同步代码到 Astore";
  syncBtn.onclick = handleSync;

  const publishBtn = document.createElement("button");
  publishBtn.className = "astore-btn astore-btn-primary";
  publishBtn.textContent = "发布";
  publishBtn.title = "发布到当前环境";
  publishBtn.onclick = handlePublish;

  const refreshBtn = document.createElement("button");
  refreshBtn.className = "astore-btn astore-btn-small";
  refreshBtn.textContent = "刷新发布结果";
  refreshBtn.onclick = handleRefresh;

  const envLabel = el("span", "astore-env-label");
  envLabel.textContent = session!.currentEnv || "日常环境";
  envLabel.id = "astore-env-display";

  actionsRow.append(syncBtn, publishBtn, envLabel, refreshBtn);
  panelEl.append(actionsRow);

  // Editor link
  const linkRow = el("div", "astore-link-row");
  const editorLink = document.createElement("a");
  editorLink.className = "astore-link";
  editorLink.textContent = "在 Astore 编辑器中打开";
  editorLink.href = "#";
  editorLink.onclick = async (e) => {
    e.preventDefault();
    try {
      const url = await astoreGetEditorUrl();
      appendMessage("info", `编辑器地址: ${url}`);
    } catch (err) {
      appendMessage("error", String(err));
    }
  };
  linkRow.append(editorLink);
  panelEl.append(linkRow);
}

async function handleLogin() {
  const uEl = document.getElementById("astore-username") as HTMLInputElement;
  const pEl = document.getElementById("astore-password") as HTMLInputElement;
  if (!uEl || !pEl) return;
  const username = uEl.value.trim();
  const password = pEl.value;
  if (!username || !password) {
    appendMessage("error", "请输入账号和密码");
    return;
  }

  appendMessage("info", "正在登录...");
  try {
    session = await astoreLogin(username, password);
    appendMessage("info", `登录成功！欢迎 ${session.nickName}`);
    if (currentProjectPath) {
      session = await astoreDetectProject(currentProjectPath);
    }
    render();
  } catch (err) {
    appendMessage("error", `登录失败: ${err}`);
  }
}

async function handleLogout() {
  try {
    await astoreLogout();
    session = null;
    appendMessage("info", "已登出");
    render();
  } catch (err) {
    appendMessage("error", `登出失败: ${err}`);
  }
}

async function handleSync() {
  if (!currentProjectPath) {
    appendMessage("error", "未打开项目");
    return;
  }
  appendMessage("info", "开始同步代码到 Astore...");
  setStatus("同步中...");
  try {
    const gitUrl = await getGitRemoteUrl();
    const branch = await getGitBranch();
    const result = await astoreSyncCode(gitUrl, branch);
    appendMessage("info", result);
    setStatus("同步完成");
  } catch (err) {
    appendMessage("error", `同步失败: ${err}`);
    setStatus("同步失败");
  }
}

async function handlePublish() {
  appendMessage("info", `开始发布到${session?.currentEnv || "日常环境"}...`);
  setStatus("发布中...");
  try {
    const result = await astorePublish();
    appendMessage("info", result);
    setStatus("发布成功");
  } catch (err) {
    appendMessage("error", `发布失败: ${err}`);
    setStatus("发布失败");
  }
}

async function handleRefresh() {
  appendMessage("info", "刷新发布结果...");
  // Trigger re-detection to refresh change record state
  if (currentProjectPath) {
    try {
      session = await astoreDetectProject(currentProjectPath);
      render();
      appendMessage("info", "刷新完成");
    } catch (err) {
      appendMessage("error", `刷新失败: ${err}`);
    }
  }
}

function setStatus(text: string) {
  const el = document.getElementById("astore-status-text");
  if (el) el.textContent = text;
}

async function getGitRemoteUrl(): Promise<string> {
  let url = await gitRemoteUrl(currentProjectPath);
  url = url.trim().replace(":", "/").replace("git@", "http://");
  return url;
}

async function getGitBranch(): Promise<string> {
  return gitCurrentBranch(currentProjectPath);
}

// Message panel

function appendMessage(level: "info" | "error", text: string) {
  if (!messageEl) return;
  const line = document.createElement("div");
  line.className = `astore-msg astore-msg-${level}`;
  const time = new Date().toLocaleTimeString();
  line.textContent = `[${time}] ${text}`;
  messageEl.append(line);
  messageEl.scrollTop = messageEl.scrollHeight;

  // Also show the astore message panel tab
  const tab = document.querySelector(
    '.panel-tab[data-panel="astore-msg"]',
  ) as HTMLElement | null;
  if (tab) {
    tab.click();
  }
}

function el(tag: string, cls: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  return e;
}
