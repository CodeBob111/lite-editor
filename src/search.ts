import { app, currentProject } from "./state";
import { escapeHtml, showStatus, fuzzyMatch } from "./utils";
import { searchInFiles, readFile, listAllFiles } from "./tauri-api";
import { openFileAtLine, openFile } from "./file-ops";
import type { FileTree } from "./file-tree";

let fileTree: FileTree;

let searchResults: { path: string; line: number; text: string }[] = [];
let searchSelectedIndex = 0;
let searchTimeout: ReturnType<typeof setTimeout> | null = null;
let searchTotalResults = 0;
let searchShowLimit = 100;
const searchPreviewCache = new Map<string, string>();
let searchPreviewToken = 0;
let searchRequestToken = 0;

let quickOpenSelectedIndex = 0;
let quickOpenRenderFrame: number | null = null;
let quickOpenPendingQuery = "";

interface QuickOpenEntry {
  path: string;
  rel: string;
  relLower: string;
  name: string;
  dir: string;
}

let quickOpenCacheProject = "";
let quickOpenCacheSource: string[] | null = null;
let quickOpenCache: QuickOpenEntry[] = [];

export function initSearch(ft: FileTree) {
  fileTree = ft;

  const searchDialogInput = document.getElementById("search-dialog-input") as HTMLInputElement;
  searchDialogInput.addEventListener("input", () => {
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => executeSearchOverlay(), 300);
  });
  searchDialogInput.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); searchNavigate(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); searchNavigate(-1); }
    else if (e.key === "Enter") { e.preventDefault(); searchConfirm(); }
    else if (e.key === "Escape") { hideSearchOverlay(); }
  });
  document.getElementById("search-overlay")!.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).id === "search-overlay") hideSearchOverlay();
  });

  const quickOpenInput = document.getElementById("quick-open-input") as HTMLInputElement;
  quickOpenInput.addEventListener("input", () => {
    quickOpenSelectedIndex = 0;
    scheduleQuickOpenRender(quickOpenInput.value);
  });
  quickOpenInput.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); quickOpenNavigate(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); quickOpenNavigate(-1); }
    else if (e.key === "Enter") { e.preventDefault(); quickOpenConfirm(); }
    else if (e.key === "Escape") { hideQuickOpen(); }
  });
  document.getElementById("quick-open-overlay")!.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).id === "quick-open-overlay") hideQuickOpen();
  });
}

// ---- Search overlay ----

export function showSearchOverlay() {
  // 两个搜索浮层互斥:打开「文件内搜索」前先关掉「快速打开」,
  // 二者同为全屏 .overlay(z-index 相同),共存会相互遮挡(见 hideQuickOpen)。
  hideQuickOpen();
  const overlay = document.getElementById("search-overlay")!;
  const input = document.getElementById("search-dialog-input") as HTMLInputElement;
  overlay.classList.remove("hidden");
  input.focus();
  input.select();
}

export function hideSearchOverlay() {
  document.getElementById("search-overlay")!.classList.add("hidden");
  document.getElementById("search-preview")!.innerHTML = "";
  searchPreviewCache.clear();
}

// 在结果区显示一行提示(搜索中 / 无结果 / 失败)。用 textContent + replaceChildren,
// 不走 innerHTML。
function showSearchMessage(text: string) {
  const container = document.getElementById("search-dialog-results")!;
  const row = document.createElement("div");
  row.className = "search-result-info";
  row.textContent = text;
  container.replaceChildren(row);
}

async function executeSearchOverlay() {
  if (!app.currentProjectPath) return;
  const token = ++searchRequestToken;
  const input = document.getElementById("search-dialog-input") as HTMLInputElement;
  const query = input.value.trim();
  if (!query) {
    document.getElementById("search-dialog-results")!.replaceChildren();
    document.getElementById("search-preview")!.replaceChildren();
    searchResults = [];
    searchTotalResults = 0;
    return;
  }

  const caseSensitive = (document.querySelector("#search-dialog-case input") as HTMLInputElement).checked;

  // 立刻给出「搜索中」反馈:大仓搜索可能要几秒,没有它时空白结果区与「没搜到」
  // 无法区分(用户此前正是因此误以为没搜到)。startedAt 用于回报真实耗时。
  showSearchMessage("Searching…");
  const startedAt = performance.now();

  try {
    const results = await searchInFiles(app.currentProjectPath, query, caseSensitive);
    if (token !== searchRequestToken) return;
    const elapsed = Math.round(performance.now() - startedAt);
    searchResults = results.map((r) => ({ path: r.path, line: r.line, text: r.text }));
    searchTotalResults = results.length;
    searchSelectedIndex = 0;
    searchShowLimit = 100;
    renderSearchResults();
    showStatus(`${results.length} result(s) in ${elapsed} ms`);
  } catch (e) {
    if (token === searchRequestToken) showSearchMessage(`Search failed: ${e}`);
    showStatus(`Search failed: ${e}`, true);
  }
}

// 等宽字体下单个字符的宽度(用于估算一行能放多少字符)。canvas 测一次后缓存。
let _searchPathCharW = 0;
function pathCharWidth(): number {
  if (_searchPathCharW) return _searchPathCharW;
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return 7.2;
  ctx.font = "12px 'SF Mono', 'Fira Code', 'Cascadia Code', monospace";
  _searchPathCharW = ctx.measureText("0".repeat(100)).width / 100 || 7.2;
  return _searchPathCharW;
}

// 路径太长放不下时省略「前面」、保留尾部(文件名最有用):头部加「…」。
function truncatePathFront(path: string, availPx: number): string {
  if (availPx <= 0) return path;
  const maxChars = Math.floor(availPx / pathCharWidth()) - 1; // 留 1 字余量,确保尾部不被裁
  if (maxChars <= 1 || path.length <= maxChars) return path;
  return "…" + path.slice(path.length - (maxChars - 1));
}

function renderSearchResults() {
  const container = document.getElementById("search-dialog-results")!;
  container.replaceChildren();
  if (!app.currentProjectPath) return;
  // 明确的「无结果」态:否则空结果区与「尚未搜索」长得一模一样(用户此前的核心困惑)。
  if (searchTotalResults === 0) {
    const row = document.createElement("div");
    row.className = "search-result-info";
    row.textContent = "No results";
    container.appendChild(row);
    document.getElementById("search-preview")!.replaceChildren();
    return;
  }
  const projectPrefix = app.currentProjectPath + "/";

  const shown = searchResults.slice(0, searchShowLimit);
  searchSelectedIndex = Math.min(searchSelectedIndex, Math.max(0, shown.length - 1));

  // 行宽固定(对话框 720px):减去行内左右 padding(18×2)与滚动条余量,得到路径可用像素宽。
  const avail = container.clientWidth - 36 - 12;

  for (let i = 0; i < shown.length; i++) {
    const r = shown[i];
    const relPath = r.path.replace(projectPrefix, "");
    const item = document.createElement("div");
    item.className = `search-result-item${i === searchSelectedIndex ? " selected" : ""}`;
    // 只展示路径;过长时省略「前面」、保留尾部文件名。完整路径放进 title 供 hover。
    const fileSpan = document.createElement("span");
    fileSpan.className = "search-result-file";
    fileSpan.textContent = truncatePathFront(relPath, avail);
    fileSpan.title = relPath;
    item.appendChild(fileSpan);
    item.addEventListener("click", () => {
      searchSelectedIndex = i;
      container.querySelectorAll(".search-result-item").forEach((el, j) => el.classList.toggle("selected", j === i));
      updateSearchPreview();
    });
    item.addEventListener("dblclick", () => {
      hideSearchOverlay();
      openFileAtLine(r.path, r.line + 1);
    });
    container.appendChild(item);
  }

  if (searchTotalResults > searchShowLimit) {
    const info = document.createElement("div");
    info.className = "search-result-info";
    info.textContent = `Showing ${searchShowLimit} of ${searchTotalResults} results`;
    const showMore = document.createElement("span");
    showMore.textContent = " — Show more";
    showMore.style.cursor = "pointer";
    showMore.style.color = "var(--accent)";
    showMore.addEventListener("click", () => {
      searchShowLimit += 100;
      renderSearchResults();
    });
    info.appendChild(showMore);
    container.appendChild(info);
  }

  updateSearchPreview();
}

function searchNavigate(delta: number) {
  const container = document.getElementById("search-dialog-results")!;
  const items = container.querySelectorAll(".search-result-item");
  if (items.length === 0) return;
  searchSelectedIndex = Math.max(0, Math.min(items.length - 1, searchSelectedIndex + delta));
  items.forEach((el, i) => el.classList.toggle("selected", i === searchSelectedIndex));
  items[searchSelectedIndex]?.scrollIntoView({ block: "nearest" });
  updateSearchPreview();
}

async function updateSearchPreview() {
  const token = ++searchPreviewToken;
  const preview = document.getElementById("search-preview")!;
  const shown = searchResults.slice(0, searchShowLimit);
  if (searchSelectedIndex < 0 || searchSelectedIndex >= shown.length) {
    preview.innerHTML = "";
    return;
  }

  const r = shown[searchSelectedIndex];
  const contextLines = 5;

  let content: string;
  if (searchPreviewCache.has(r.path)) {
    content = searchPreviewCache.get(r.path)!;
  } else {
    try {
      content = await readFile(r.path);
      if (token !== searchPreviewToken) return;
      searchPreviewCache.set(r.path, content);
    } catch {
      if (token !== searchPreviewToken) return;
      preview.innerHTML = '<div class="search-preview-empty">Cannot read file</div>';
      return;
    }
  }

  const lines = content.split("\n");
  const start = Math.max(0, r.line - contextLines);
  const end = Math.min(lines.length, r.line + contextLines + 1);
  const query = (document.getElementById("search-dialog-input") as HTMLInputElement).value.trim();

  let html = '<div class="search-preview-lines">';
  for (let i = start; i < end; i++) {
    const lineNum = i + 1;
    const isMatch = i === r.line;
    let lineText = escapeHtml(lines[i]);
    if (isMatch && query) {
      const caseSensitive = (document.querySelector("#search-dialog-case input") as HTMLInputElement).checked;
      const flags = caseSensitive ? "g" : "gi";
      const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      lineText = lineText.replace(new RegExp(escaped, flags), (m) => `<mark>${m}</mark>`);
    }
    html += `<div class="search-preview-line${isMatch ? " search-preview-match" : ""}"><span class="search-preview-num">${lineNum}</span><span class="search-preview-code">${lineText}</span></div>`;
  }
  html += "</div>";

  const relPath = app.currentProjectPath ? r.path.replace(app.currentProjectPath + "/", "") : r.path;
  preview.innerHTML = `<div class="search-preview-header">${escapeHtml(relPath)}:${r.line + 1}</div>${html}`;
}

function searchConfirm() {
  const shown = searchResults.slice(0, searchShowLimit);
  if (searchSelectedIndex >= 0 && searchSelectedIndex < shown.length) {
    const r = shown[searchSelectedIndex];
    hideSearchOverlay();
    openFileAtLine(r.path, r.line + 1);
  }
}

// ---- Quick open ----

export function showQuickOpen() {
  // 与 showSearchOverlay 对称:打开「快速打开」前先关掉「文件内搜索」,
  // 避免双击 Shift 在 Cmd+Shift+F 浮层之上再叠一个把输入框挡住。
  hideSearchOverlay();
  const overlay = document.getElementById("quick-open-overlay")!;
  const input = document.getElementById("quick-open-input") as HTMLInputElement;
  overlay.classList.remove("hidden");
  input.value = "";
  quickOpenSelectedIndex = 0;
  ensureQuickOpenCache();
  renderQuickOpenResults("");
  input.focus();
}

export function hideQuickOpen() {
  document.getElementById("quick-open-overlay")!.classList.add("hidden");
}

function scheduleQuickOpenRender(query: string) {
  quickOpenPendingQuery = query;
  if (quickOpenRenderFrame !== null) return;
  quickOpenRenderFrame = requestAnimationFrame(() => {
    quickOpenRenderFrame = null;
    renderQuickOpenResults(quickOpenPendingQuery);
  });
}

let fileListLoading = false;

function ensureQuickOpenCache(): QuickOpenEntry[] {
  const project = currentProject();
  if (!app.currentProjectPath) return [];
  const source = project?.allFilePathsCache || fileTree.getAllFiles();

  if (!project?.allFilePathsCache && !fileListLoading && app.currentProjectPath) {
    fileListLoading = true;
    const pp = app.currentProjectPath;
    listAllFiles(pp)
      .then((files) => {
        if (project) {
          project.allFilePathsCache = files;
          quickOpenCacheProject = "";
        }
        fileListLoading = false;
      })
      .catch(() => { fileListLoading = false; });
  }

  if (
    quickOpenCacheProject === app.currentProjectPath &&
    quickOpenCacheSource === source &&
    quickOpenCache.length > 0
  ) {
    return quickOpenCache;
  }

  const projectPrefix = app.currentProjectPath + "/";
  quickOpenCacheProject = app.currentProjectPath;
  quickOpenCacheSource = source;
  quickOpenCache = source.map((path) => {
    const rel = path.replace(projectPrefix, "");
    const name = path.split("/").pop() || path;
    const dir = rel.includes("/") ? rel.substring(0, rel.lastIndexOf("/")) : "";
    return { path, rel, relLower: rel.toLowerCase(), name, dir };
  });
  return quickOpenCache;
}

function renderQuickOpenResults(query: string) {
  const container = document.getElementById("quick-open-results")!;
  container.innerHTML = "";

  if (!app.currentProjectPath) return;
  const entries = ensureQuickOpenCache();
  const q = query.trim();

  let shown: QuickOpenEntry[];
  if (q) {
    const qLower = q.toLowerCase();
    const best: { entry: QuickOpenEntry; score: number }[] = [];
    for (const entry of entries) {
      const score = fuzzyMatch(qLower, entry.relLower);
      if (score <= 0) continue;
      let insertAt = best.length;
      while (insertAt > 0 && best[insertAt - 1].score < score) insertAt--;
      if (insertAt < 50) {
        best.splice(insertAt, 0, { entry, score });
        if (best.length > 50) best.pop();
      }
    }
    shown = best.map((s) => s.entry);
  } else {
    shown = entries.slice(0, 50);
  }

  quickOpenSelectedIndex = Math.min(quickOpenSelectedIndex, shown.length - 1);

  for (let i = 0; i < shown.length; i++) {
    const entry = shown[i];

    const item = document.createElement("div");
    item.className = `quick-open-item${i === quickOpenSelectedIndex ? " selected" : ""}`;
    item.dataset.path = entry.path;
    item.innerHTML = `<span class="quick-open-filename">${escapeHtml(entry.name)}</span><span class="quick-open-path">${escapeHtml(entry.dir)}</span>`;
    item.addEventListener("click", () => {
      hideQuickOpen();
      openFile(entry.path);
    });
    container.appendChild(item);
  }
}

function quickOpenNavigate(delta: number) {
  const container = document.getElementById("quick-open-results")!;
  const items = container.querySelectorAll(".quick-open-item");
  if (items.length === 0) return;

  quickOpenSelectedIndex = Math.max(0, Math.min(items.length - 1, quickOpenSelectedIndex + delta));
  items.forEach((el, i) => el.classList.toggle("selected", i === quickOpenSelectedIndex));
  items[quickOpenSelectedIndex]?.scrollIntoView({ block: "nearest" });
}

function quickOpenConfirm() {
  const container = document.getElementById("quick-open-results")!;
  const items = container.querySelectorAll(".quick-open-item");
  if (quickOpenSelectedIndex >= 0 && quickOpenSelectedIndex < items.length) {
    (items[quickOpenSelectedIndex] as HTMLElement).click();
  }
}
