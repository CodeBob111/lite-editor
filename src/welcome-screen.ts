import { app } from "./state";
import { abbreviatePath } from "./utils";
import { loadRecentProjects } from "./recent-projects";

// 欢迎/起始屏(设计稿 B):覆盖编辑区,无打开文件时显示。
// hero + START 操作 + RECENT 最近项目 + 功能卡片。沿用窗口 chrome(项目条/侧栏/状态栏)。
// 操作绑定真实函数(经 init 注入),最近项目取 localStorage 的真实记录。

interface WelcomeActions {
  onOpenFolder: () => void;
  onClone: () => void;
  onNewTerminal: () => void;
  onOpenRecent: (path: string) => void;
}

let actions: WelcomeActions | null = null;
let screen: HTMLElement | null = null;
let recentListEl: HTMLElement | null = null;

export function initWelcomeScreen(a: WelcomeActions) {
  actions = a;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K, cls?: string, text?: string,
): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

function relTime(ts: number): string {
  if (!ts) return "";
  const s = Math.max(0, (Date.now() - ts) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  const d = Math.floor(s / 86400);
  if (d === 1) return "yesterday";
  if (d < 7) return `${d} days ago`;
  if (d < 30) return `${Math.floor(d / 7)} weeks ago`;
  return `${Math.floor(d / 30)} months ago`;
}

function action(icon: string, title: string, desc: string, onClick: () => void): HTMLElement {
  const a = el("button", "welcome-action");
  a.append(el("span", "welcome-action-ico", icon));
  const t = el("div", "welcome-action-text");
  t.append(el("div", "welcome-action-title", title));
  t.append(el("div", "welcome-action-desc", desc));
  a.append(t);
  a.addEventListener("click", onClick);
  return a;
}

function card(icon: string, title: string, desc: string): HTMLElement {
  const c = el("div", "welcome-card");
  const h = el("div", "welcome-card-head");
  h.append(el("span", "welcome-card-ico", icon));
  h.append(el("span", "welcome-card-title", title));
  c.append(h);
  c.append(el("div", "welcome-card-desc", desc));
  return c;
}

function build() {
  const area = document.getElementById("editor-area");
  if (!area) return;
  screen = el("div", "welcome-screen hidden");
  screen.id = "welcome-screen";

  const inner = el("div", "welcome-inner");

  // hero
  const hero = el("div", "welcome-hero");
  const logo = el("div", "welcome-logo", "</>");
  const heroText = el("div", "welcome-hero-text");
  const titleLine = el("div", "welcome-title-line");
  titleLine.append(el("span", "welcome-title", "Lite Editor"));
  titleLine.append(el("span", "welcome-version", "v0.1.0"));
  heroText.append(titleLine);
  heroText.append(el("div", "welcome-tagline",
    "A native desktop code editor — Tauri 2 + CodeMirror 6. Fast multi-tab editing, a full Git client, Java intelligence, and a built-in terminal in one small binary."));
  hero.append(logo, heroText);
  inner.append(hero);

  // columns: START + RECENT
  const cols = el("div", "welcome-cols");

  const start = el("div", "welcome-col");
  start.append(el("div", "welcome-col-head", "START"));
  start.append(action("📁", "Open Folder…", "Open a project workspace", () => actions?.onOpenFolder()));
  start.append(action("⎇", "Clone Repository…", "Clone from a Git remote", () => actions?.onClone()));
  start.append(action("▸_", "New Terminal", "portable-pty session", () => actions?.onNewTerminal()));
  cols.append(start);

  const recent = el("div", "welcome-col");
  recent.append(el("div", "welcome-col-head", "RECENT"));
  recentListEl = el("div", "welcome-recent-list");
  recent.append(recentListEl);
  cols.append(recent);

  inner.append(cols);

  // feature cards
  const cardsHead = el("div", "welcome-cards-head", "BUILT FOR JAVA & MAVEN WORKFLOWS");
  inner.append(cardsHead);
  const cards = el("div", "welcome-cards");
  cards.append(card("✦", "Language intelligence", "jdtls go-to-definition, find usages, and inline diagnostics."));
  cards.append(card("⎇", "Full Git client", "Hunk-level staging, inline blame, branches, and merge tools."));
  cards.append(card("↻", "Reload on change", "A native filesystem watcher reloads files edited outside."));
  inner.append(cards);

  screen.append(inner);
  area.appendChild(screen);
}

function renderRecents() {
  if (!recentListEl) return;
  recentListEl.replaceChildren();
  const recents = loadRecentProjects();
  if (recents.length === 0) {
    recentListEl.append(el("div", "welcome-recent-empty", "No recent projects yet."));
    return;
  }
  for (const r of recents) {
    const row = el("button", "welcome-recent-item");
    row.append(el("span", "welcome-recent-name", r.name));
    row.append(el("span", "welcome-recent-path", abbreviatePath(r.path)));
    row.append(el("span", "welcome-recent-time", relTime(r.openedAt)));
    row.addEventListener("click", () => actions?.onOpenRecent(r.path));
    recentListEl.append(row);
  }
}

// 无打开文件(或欢迎伪文件)时显示欢迎屏,否则隐藏。在 onTabActivate / onAllClosed /
// 启动处调用。
export function refreshWelcomeScreen() {
  if (!screen) build();
  if (!screen) return;
  const file = app.currentFilePath;
  const show = !file || file === "welcome.ts";
  if (show) {
    renderRecents();
    screen.classList.remove("hidden");
  } else {
    screen.classList.add("hidden");
  }
}
