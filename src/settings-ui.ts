import {
  EDITOR_SETTINGS, getSetting, setSetting, allSettings, replaceAllSettings,
  type SettingsSchemaItem,
} from "./settings";

// 设置屏(对齐设计稿 C):顶部 Settings / settings.json 双标签,左侧分类导航,
// 右侧设置项;settings.json 标签是同一份配置的原始 JSON 视图,双向绑定。
// 本编辑器目前只有 Editor 一类真实设置(其余分类无对应功能,按用户决定不造死 UI)。

let overlay: HTMLElement | null = null;
let activeTab: "ui" | "json" = "ui";
let listEl: HTMLElement | null = null;
let jsonArea: HTMLTextAreaElement | null = null;
let jsonStatus: HTMLElement | null = null;

export function isSettingsOpen(): boolean {
  return !!overlay && !overlay.classList.contains("hidden");
}

export function closeSettings(): void {
  overlay?.classList.add("hidden");
}

export function openSettings(): void {
  if (!overlay) build();
  overlay!.classList.remove("hidden");
  switchTab("ui");
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K, cls?: string, text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function build(): void {
  overlay = el("div", "settings-overlay hidden");
  overlay.id = "settings-overlay";

  // 顶栏:双标签 + 关闭
  const top = el("div", "settings-top");
  const tabUi = el("button", "settings-tab active", "Settings");
  tabUi.dataset.tab = "ui";
  const tabJson = el("button", "settings-tab", "{ } settings.json");
  tabJson.dataset.tab = "json";
  tabUi.addEventListener("click", () => switchTab("ui"));
  tabJson.addEventListener("click", () => switchTab("json"));
  const spacer = el("div", "settings-top-spacer");
  const close = el("button", "settings-close", "✕");
  close.title = "Close (Esc)";
  close.addEventListener("click", closeSettings);
  top.append(tabUi, tabJson, spacer, close);

  // Settings 面板:搜索 + 左导航 + 右列表
  const uiPane = el("div", "settings-pane settings-pane-ui");
  uiPane.dataset.pane = "ui";
  const searchWrap = el("div", "settings-search-wrap");
  const search = el("input", "settings-search");
  search.type = "text";
  search.placeholder = "Search settings";
  search.spellcheck = false;
  search.addEventListener("input", () => renderList(search.value.trim().toLowerCase()));
  searchWrap.append(search);

  const body = el("div", "settings-body");
  const nav = el("div", "settings-nav");
  const navHead = el("div", "settings-nav-head", "COMMONLY USED");
  const navEditor = el("button", "settings-nav-item active", "Editor");
  nav.append(navHead, navEditor);

  listEl = el("div", "settings-list");
  body.append(nav, listEl);
  uiPane.append(searchWrap, body);

  // settings.json 面板:原始 JSON,合法即落盘并回灌控件
  const jsonPane = el("div", "settings-pane settings-pane-json hidden");
  jsonPane.dataset.pane = "json";
  jsonArea = el("textarea", "settings-json-area") as HTMLTextAreaElement;
  jsonArea.spellcheck = false;
  jsonStatus = el("div", "settings-json-status");
  jsonArea.addEventListener("input", onJsonInput);
  jsonPane.append(jsonArea, jsonStatus);

  overlay.append(top, uiPane, jsonPane);
  document.body.appendChild(overlay);

  overlay.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.stopPropagation(); closeSettings(); }
  });

  renderList("");
}

function switchTab(tab: "ui" | "json"): void {
  activeTab = tab;
  overlay!.querySelectorAll<HTMLElement>(".settings-tab").forEach((t) =>
    t.classList.toggle("active", t.dataset.tab === tab));
  overlay!.querySelectorAll<HTMLElement>(".settings-pane").forEach((p) =>
    p.classList.toggle("hidden", p.dataset.pane !== tab));
  if (tab === "json") renderJson();
  else renderList("");
}

function renderList(filter: string): void {
  if (!listEl) return;
  listEl.replaceChildren();
  const head = el("div", "settings-section-head");
  head.append(el("div", "settings-section-title", "Editor"));
  head.append(el("div", "settings-section-sub", "Controls the CodeMirror 6 editing surface."));
  listEl.append(head);

  const items = EDITOR_SETTINGS.filter((it) =>
    !filter || it.label.toLowerCase().includes(filter) || it.key.toLowerCase().includes(filter));
  for (const item of items) listEl.append(renderRow(item));
}

function renderRow(item: SettingsSchemaItem): HTMLElement {
  const row = el("div", "settings-row");
  const info = el("div", "settings-row-info");
  const labelLine = el("div", "settings-row-label");
  labelLine.append(el("span", "settings-row-name", item.label));
  labelLine.append(el("span", "settings-row-key", item.key));
  info.append(labelLine);
  info.append(el("div", "settings-row-desc", item.desc));
  row.append(info);
  row.append(renderControl(item));
  return row;
}

function renderControl(item: SettingsSchemaItem): HTMLElement {
  const ctl = el("div", "settings-control");
  if (item.type === "toggle") {
    const t = el("button", "set-toggle");
    const on = !!getSetting<boolean>(item.key);
    t.classList.toggle("on", on);
    t.append(el("span", "set-toggle-knob"));
    t.addEventListener("click", () => {
      const next = !t.classList.contains("on");
      t.classList.toggle("on", next);
      setSetting(item.key, next);
    });
    ctl.append(t);
  } else if (item.type === "segment") {
    const group = el("div", "set-segment");
    const cur = getSetting<number | string>(item.key);
    for (const opt of item.options ?? []) {
      const b = el("button", "set-seg-btn", String(opt));
      if (opt === cur) b.classList.add("active");
      b.addEventListener("click", () => {
        group.querySelectorAll(".set-seg-btn").forEach((x) => x.classList.remove("active"));
        b.classList.add("active");
        setSetting(item.key, opt);
      });
      group.append(b);
    }
    ctl.append(group);
  } else if (item.type === "select") {
    const sel = el("select", "set-select") as HTMLSelectElement;
    const cur = String(getSetting(item.key));
    for (const opt of item.options ?? []) {
      const o = el("option", undefined, String(opt)) as HTMLOptionElement;
      o.value = String(opt);
      if (String(opt) === cur) o.selected = true;
      sel.append(o);
    }
    sel.addEventListener("change", () => {
      const raw = sel.value;
      const num = Number(raw);
      setSetting(item.key, Number.isNaN(num) ? raw : num);
    });
    ctl.append(sel);
  } else {
    const inp = el("input", "set-text") as HTMLInputElement;
    inp.type = "text";
    inp.value = String(getSetting(item.key));
    inp.spellcheck = false;
    inp.addEventListener("change", () => setSetting(item.key, inp.value));
    ctl.append(inp);
  }
  return ctl;
}

function renderJson(): void {
  if (!jsonArea || !jsonStatus) return;
  jsonArea.value = JSON.stringify(allSettings(), null, 2);
  jsonStatus.textContent = "";
  jsonStatus.className = "settings-json-status";
}

function onJsonInput(): void {
  if (!jsonArea || !jsonStatus) return;
  const ok = replaceAllSettings(jsonArea.value);
  if (ok) {
    jsonStatus.textContent = "Saved";
    jsonStatus.className = "settings-json-status ok";
  } else {
    jsonStatus.textContent = "Invalid JSON";
    jsonStatus.className = "settings-json-status err";
  }
}
