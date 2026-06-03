import { app } from "./state";
import { escapeHtml, showStatus } from "./utils";
import { appConfirm } from "./dialog";
import { showConflictsDialog } from "./merge-conflict";
import {
  gitDiscoverRepos, gitStatusBatch, gitShowFile, gitShowStaged, readFile,
  gitCommit, gitPush, gitCurrentBranch, gitDiscardChanges,
  type GitChange,
} from "./tauri-api";

interface RepoChanges {
  name: string;
  path: string;
  changes: GitChange[];
  expanded: boolean;
}

type ChangesRow =
  | { kind: "repo"; repoIndex: number; top: number; height: number }
  | { kind: "section"; repoIndex: number; label: string; top: number; height: number }
  | { kind: "file"; repoIndex: number; changeIndex: number; top: number; height: number };

let repoChanges: RepoChanges[] = [];
let openFileFn: ((path: string) => void) | null = null;
let openDiffFn: ((repoPath: string, change: GitChange, original: string, modified: string) => void) | null = null;
let changesContainer: HTMLElement | null = null;
let changesRows: ChangesRow[] = [];
let changesTotalHeight = 0;
let virtualFrame: number | null = null;
let visibleRangeKey = "";
let changesRenderFrame: number | null = null;
let loadInFlight: Promise<void> | null = null;
let loadInFlightProject = "";
let loadToken = 0;
let lastLoadedAt = 0;
let lastLoadedProject = "";
let hasLoadedChanges = false;
let changesLoading = false;
let diffOpenToken = 0;
const diffCache = new Map<string, { original: string; modified: string }>();
const DIFF_CACHE_MAX = 40;
const CHANGES_CACHE_MS = 2500;
const REPO_CACHE_MS = 30_000;
const selectedFiles = new Set<string>();
let repoCacheProject = "";
let repoCacheLoadedAt = 0;
let repoCache: { name: string; path: string }[] = [];
const ROW_HEIGHT = {
  repo: 28,
  section: 21,
  file: 25,
};

const statusColors: Record<string, string> = {
  Modified: "#6897bb",
  Added: "#6a8759",
  Deleted: "#cc7832",
  Renamed: "#ffc66d",
  Untracked: "#808080",
  Copied: "#6897bb",
  Unmerged: "#d45555",
};

const statusLetters: Record<string, string> = {
  Modified: "M",
  Added: "A",
  Deleted: "D",
  Renamed: "R",
  Untracked: "U",
  Copied: "C",
  Unmerged: "!",
};

function fileKey(repoPath: string, filePath: string): string {
  return `${repoPath}\0${filePath}`;
}

function updateCommitUI() {
  const countEl = document.getElementById("changes-selected-count");
  const btnCommit = document.getElementById("changes-btn-commit") as HTMLButtonElement | null;
  const btnPush = document.getElementById("changes-btn-commit-push") as HTMLButtonElement | null;
  const msgEl = document.getElementById("changes-commit-msg") as HTMLTextAreaElement | null;
  const n = selectedFiles.size;
  if (countEl) countEl.textContent = n > 0 ? `${n} selected` : "";
  const hasMsg = !!msgEl?.value.trim();
  const canCommit = n > 0 && hasMsg;
  if (btnCommit) btnCommit.disabled = !canCommit;
  if (btnPush) btnPush.disabled = !canCommit;
}

function selectAllFiles() {
  for (const repo of repoChanges) {
    for (const c of repo.changes) {
      selectedFiles.add(fileKey(repo.path, c.path));
    }
  }
}

function getFileIcon(filename: string): [string, string] {
  const ext = filename.split(".").pop()?.toLowerCase() || "";
  const map: Record<string, [string, string]> = {
    java: ["J", "#cc7832"],
    xml: ["X", "#9876aa"],
    md: ["M", "#6897bb"],
    ts: ["T", "#519aba"],
    tsx: ["T", "#519aba"],
    js: ["J", "#cbcb41"],
    json: ["{}", "#cbcb41"],
    css: ["#", "#56b6c2"],
    html: ["H", "#e37933"],
    properties: ["P", "#a0a0a0"],
    yml: ["Y", "#a0a0a0"],
    yaml: ["Y", "#a0a0a0"],
  };
  return map[ext] || ["·", "#a0a0a0"];
}

export async function loadChanges(force = false) {
  if (!app.currentProjectPath) return;
  const container = changesContainer ?? document.getElementById("changes-list")!;
  const now = performance.now();
  const projectPath = app.currentProjectPath;
  const projectChanged = lastLoadedProject !== projectPath;
  if (force || projectChanged) {
    diffCache.clear();
    repoCacheProject = "";
    repoChanges = [];
    changesRows = [];
    changesTotalHeight = 0;
    visibleRangeKey = "";
    hasLoadedChanges = false;
    changesLoading = true;
    renderChanges(container);
  }
  if (!force && loadInFlight && loadInFlightProject === projectPath) return loadInFlight;
  if (!force && hasLoadedChanges && now - lastLoadedAt < CHANGES_CACHE_MS) {
    renderChanges(container);
    return;
  }
  if (!force && hasLoadedChanges) {
    renderChanges(container);
  }

  const token = ++loadToken;
  loadInFlightProject = projectPath;
  loadInFlight = (async () => {
    const repos = await discoverChangesRepos(projectPath);
    if (token !== loadToken || app.currentProjectPath !== projectPath) return;

    const previous = repoChanges;
    repoChanges = repos.map((repo) => {
      const existing = previous.find((r) => r.path === repo.path);
      return {
        name: repo.name,
        path: repo.path,
        changes: existing?.changes ?? [],
        expanded: existing?.expanded ?? true,
      };
    });
    changesLoading = true;
    renderChanges(container);

    try {
      const batchResults = await gitStatusBatch(repos.map((r) => r.path));
      if (token !== loadToken || app.currentProjectPath !== projectPath) return;
      for (const br of batchResults) {
        const index = repos.findIndex((r) => r.path === br.path);
        if (index < 0) continue;
        const existing = repoChanges[index];
        if (!existing || existing.path !== br.path) continue;
        if (br.result) repoChanges[index] = { ...existing, changes: br.result };
      }
      scheduleChangesRender(container);
    } catch {
      // batch failed, keep previous
    }

    if (token !== loadToken || app.currentProjectPath !== projectPath) return;
    changesLoading = false;
    hasLoadedChanges = true;
    lastLoadedProject = projectPath;
    lastLoadedAt = performance.now();
    updateCommitUI();
    renderChanges(container);
  })().finally(() => {
    if (token === loadToken) {
      loadInFlight = null;
      loadInFlightProject = "";
    }
  });

  return loadInFlight;
}

async function discoverChangesRepos(projectPath: string) {
  const now = performance.now();
  if (repoCacheProject === projectPath && now - repoCacheLoadedAt < REPO_CACHE_MS) {
    return repoCache;
  }
  const repos = await gitDiscoverRepos(projectPath);
  repoCacheProject = projectPath;
  repoCacheLoadedAt = performance.now();
  repoCache = repos;
  return repos;
}

function scheduleChangesRender(container: HTMLElement) {
  if (changesRenderFrame !== null) return;
  changesRenderFrame = requestAnimationFrame(() => {
    changesRenderFrame = null;
    renderChanges(container);
  });
}

function renderChanges(container: HTMLElement) {
  const totalFiles = repoChanges.reduce((n, r) => n + r.changes.length, 0);
  if (totalFiles === 0) {
    container.innerHTML = `<div class="panel-placeholder">${changesLoading ? "Loading changes..." : "No changes"}</div>`;
    changesRows = [];
    changesTotalHeight = 0;
    visibleRangeKey = "";
    return;
  }

  buildRows();
  visibleRangeKey = "";
  container.innerHTML = '<div class="changes-virtual-spacer"><div class="changes-virtual-content"></div></div>';
  const spacer = container.querySelector<HTMLElement>(".changes-virtual-spacer")!;
  spacer.style.height = `${changesTotalHeight}px`;
  updateVisibleRows(container);
}

function buildRows() {
  changesRows = [];
  let top = 0;

  for (let repoIndex = 0; repoIndex < repoChanges.length; repoIndex++) {
    const repo = repoChanges[repoIndex];
    if (repo.changes.length === 0) continue;

    changesRows.push({ kind: "repo", repoIndex, top, height: ROW_HEIGHT.repo });
    top += ROW_HEIGHT.repo;

    if (repo.expanded) {
      const staged = repo.changes
        .map((change, changeIndex) => ({ change, changeIndex }))
        .filter(({ change }) => change.staged);
      const unstaged = repo.changes
        .map((change, changeIndex) => ({ change, changeIndex }))
        .filter(({ change }) => !change.staged);

      if (staged.length > 0) {
        changesRows.push({ kind: "section", repoIndex, label: "Staged", top, height: ROW_HEIGHT.section });
        top += ROW_HEIGHT.section;
        for (const { changeIndex } of staged) {
          changesRows.push({ kind: "file", repoIndex, changeIndex, top, height: ROW_HEIGHT.file });
          top += ROW_HEIGHT.file;
        }
      }
      if (unstaged.length > 0) {
        if (staged.length > 0) {
          changesRows.push({ kind: "section", repoIndex, label: "Changes", top, height: ROW_HEIGHT.section });
          top += ROW_HEIGHT.section;
        }
        for (const { changeIndex } of unstaged) {
          changesRows.push({ kind: "file", repoIndex, changeIndex, top, height: ROW_HEIGHT.file });
          top += ROW_HEIGHT.file;
        }
      }
    }
  }

  changesTotalHeight = top;
}

function scheduleVisibleRowsUpdate() {
  if (!changesContainer || virtualFrame !== null) return;
  virtualFrame = requestAnimationFrame(() => {
    virtualFrame = null;
    if (changesContainer) updateVisibleRows(changesContainer);
  });
}

function updateVisibleRows(container: HTMLElement) {
  const content = container.querySelector<HTMLElement>(".changes-virtual-content");
  if (!content) return;

  const scrollTop = container.scrollTop;
  const viewportHeight = container.clientHeight;
  const overscan = 220;
  const start = Math.max(0, scrollTop - overscan);
  const end = scrollTop + viewportHeight + overscan;
  const startIndex = lowerBoundRowBottom(start);
  const endIndex = upperBoundRowTop(end);
  const rangeKey = `${startIndex}:${endIndex}`;
  if (rangeKey === visibleRangeKey) return;
  visibleRangeKey = rangeKey;
  const visible = changesRows.slice(startIndex, endIndex);

  content.innerHTML = visible.map(renderRow).join("");
  content.querySelectorAll<HTMLInputElement>(".changes-repo-checkbox[data-indeterminate]").forEach((cb) => {
    cb.indeterminate = true;
  });
}

function lowerBoundRowBottom(offset: number) {
  let lo = 0;
  let hi = changesRows.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const row = changesRows[mid];
    if (row.top + row.height < offset) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function upperBoundRowTop(offset: number) {
  let lo = 0;
  let hi = changesRows.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (changesRows[mid].top <= offset) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function renderRow(row: ChangesRow): string {
  const style = `transform: translateY(${row.top}px); height:${row.height}px;`;
  const repo = repoChanges[row.repoIndex];

  if (row.kind === "repo") {
    const allChecked = repo.changes.length > 0 && repo.changes.every((c) => selectedFiles.has(fileKey(repo.path, c.path)));
    const someChecked = !allChecked && repo.changes.some((c) => selectedFiles.has(fileKey(repo.path, c.path)));
    const cbState = allChecked ? "checked" : "";
    const indeterminate = someChecked ? "data-indeterminate" : "";
    return `<div class="changes-repo-header changes-row" style="${style}" data-row-kind="repo" data-repo-index="${row.repoIndex}">
      <input type="checkbox" class="changes-repo-checkbox" ${cbState} ${indeterminate} data-repo-path="${escapeHtml(repo.path)}">
      <span class="changes-toggle">${repo.expanded ? "&#9660;" : "&#9654;"}</span>
      <span class="changes-repo-name">${escapeHtml(repo.name)}</span>
      <span class="changes-count">${repo.changes.length} file${repo.changes.length !== 1 ? "s" : ""}</span>
    </div>`;
  }

  if (row.kind === "section") {
    return `<div class="changes-section-header changes-row" style="${style}">${escapeHtml(row.label)}</div>`;
  }

  const change = repo.changes[row.changeIndex];
  return renderChangeItem(change, row.repoIndex, row.changeIndex, style);
}

async function openChangedFile(repoPath: string, change: GitChange) {
  const token = ++diffOpenToken;
  const relPath = change.path;
  const filePath = `${repoPath}/${relPath}`;
  const filename = filePath.split("/").pop() || filePath;

  if (change.status === "Untracked") {
    if (openFileFn) openFileFn(filePath);
    return;
  }

  // A conflicted file opens the 3-way merge resolver instead of a plain diff.
  if (change.status === "Unmerged") {
    try {
      const branch = await gitCurrentBranch(repoPath);
      await showConflictsDialog(repoPath, branch);
    } catch (e) {
      showStatus(`Failed to open conflicts: ${e}`, true);
    }
    return;
  }

  try {
    showStatus("Loading diff...");

    let original = "";
    let modified = "";
    const originalRelPath = change.old_path || relPath;
    const cacheKey = `${repoPath}\0${relPath}\0${originalRelPath}\0${change.status}\0${change.staged ? "1" : "0"}`;
    const cached = diffCache.get(cacheKey);

    if (cached) {
      original = cached.original;
      modified = cached.modified;
    } else {
      if (change.status === "Added") {
        if (change.staged) {
          modified = await gitShowStaged(repoPath, relPath);
        } else {
          modified = await readFile(filePath);
        }
      } else if (change.status === "Deleted") {
        original = await gitShowFile(repoPath, originalRelPath);
      } else {
        try {
          original = await gitShowFile(repoPath, originalRelPath);
        } catch {
          // file doesn't exist in HEAD
        }
        if (change.staged) {
          modified = await gitShowStaged(repoPath, relPath);
        } else {
          modified = await readFile(filePath);
        }
      }

      diffCache.set(cacheKey, { original, modified });
      if (diffCache.size > DIFF_CACHE_MAX) {
        const first = diffCache.keys().next().value;
        if (first !== undefined) diffCache.delete(first);
      }
    }

    if (token !== diffOpenToken) return;

    if (openDiffFn) openDiffFn(repoPath, change, original, modified);
    showStatus(`Diff: ${filename}`);
  } catch (e) {
    if (token !== diffOpenToken) return;
    showStatus(`Failed to load diff: ${e}`, true);
  }
}

function renderChangeItem(c: GitChange, repoIndex: number, changeIndex: number, style: string): string {
  const repo = repoChanges[repoIndex];
  const filename = c.path.split("/").pop() || c.path;
  const dir = c.path.includes("/") ? c.path.substring(0, c.path.lastIndexOf("/")) : "";
  const color = statusColors[c.status] || "#a0a0a0";
  const letter = statusLetters[c.status] || "?";
  const [iconLetter, iconColor] = getFileIcon(filename);
  const checked = selectedFiles.has(fileKey(repo.path, c.path)) ? "checked" : "";

  return `<div class="changes-file changes-row" style="${style}" data-row-kind="file" data-repo-index="${repoIndex}" data-change-index="${changeIndex}">
    <input type="checkbox" class="changes-checkbox" ${checked} data-repo-path="${escapeHtml(repo.path)}" data-file-path="${escapeHtml(c.path)}">
    <span class="changes-file-icon" style="color:${iconColor}">${iconLetter}</span>
    <span class="changes-file-name">${escapeHtml(filename)}</span>
    <span class="changes-file-dir">${escapeHtml(dir)}</span>
    <span class="changes-status" style="color:${color}">${letter}</span>
  </div>`;
}

export function closeDiff() {
  diffOpenToken++;
}

function handleChangesClick(e: MouseEvent) {
  const target = e.target as HTMLElement;

  if (target.classList.contains("changes-checkbox")) {
    const cb = target as HTMLInputElement;
    const repoPath = cb.dataset.repoPath!;
    const filePath = cb.dataset.filePath!;
    const key = fileKey(repoPath, filePath);
    if (cb.checked) selectedFiles.add(key); else selectedFiles.delete(key);
    updateCommitUI();
    if (changesContainer) { visibleRangeKey = ""; updateVisibleRows(changesContainer); }
    e.stopPropagation();
    return;
  }

  if (target.classList.contains("changes-repo-checkbox")) {
    const cb = target as HTMLInputElement;
    const repoPath = cb.dataset.repoPath!;
    const repo = repoChanges.find((r) => r.path === repoPath);
    if (repo) {
      if (cb.checked) {
        for (const c of repo.changes) selectedFiles.add(fileKey(repo.path, c.path));
      } else {
        for (const c of repo.changes) selectedFiles.delete(fileKey(repo.path, c.path));
      }
    }
    updateCommitUI();
    if (changesContainer) { visibleRangeKey = ""; updateVisibleRows(changesContainer); }
    e.stopPropagation();
    return;
  }

  const row = target.closest<HTMLElement>(".changes-row");
  if (!row) return;

  const repoIndex = Number(row.dataset.repoIndex);
  const repo = Number.isInteger(repoIndex) ? repoChanges[repoIndex] : null;
  if (!repo) return;

  if (row.dataset.rowKind === "repo") {
    repo.expanded = !repo.expanded;
    if (changesContainer) {
      buildRows();
      visibleRangeKey = "";
      const spacer = changesContainer.querySelector<HTMLElement>(".changes-virtual-spacer");
      if (spacer) spacer.style.height = `${changesTotalHeight}px`;
      updateVisibleRows(changesContainer);
    }
    return;
  }

  if (row.dataset.rowKind === "file") {
    const changeIndex = Number(row.dataset.changeIndex);
    const change = Number.isInteger(changeIndex) ? repo.changes[changeIndex] : null;
    if (change) openChangedFile(repo.path, change);
  }
}

function handleChangesContextMenu(e: MouseEvent) {
  const row = (e.target as HTMLElement).closest<HTMLElement>(".changes-file");
  if (!row) return;
  e.preventDefault();
  const repoIndex = Number(row.dataset.repoIndex);
  const repo = Number.isInteger(repoIndex) ? repoChanges[repoIndex] : null;
  if (!repo) return;
  const changeIndex = Number(row.dataset.changeIndex);
  const change = Number.isInteger(changeIndex) ? repo.changes[changeIndex] : null;
  if (!change) return;
  showChangesContextMenu(repo.path, change, e.clientX, e.clientY);
}

// Built with createElement/textContent (never innerHTML) so file paths can't
// inject markup and the security hook stays happy.
function showChangesContextMenu(repoPath: string, change: GitChange, x: number, y: number) {
  document.querySelectorAll(".changes-ctx-menu").forEach((m) => m.remove());
  const menu = document.createElement("div");
  menu.className = "changes-ctx-menu";
  menu.style.cssText = `position:fixed;left:${x}px;top:${y}px;z-index:9999;`;

  const addItem = (label: string, onClick: () => void) => {
    const item = document.createElement("div");
    item.className = "changes-ctx-item";
    item.textContent = label;
    item.addEventListener("click", () => { menu.remove(); onClick(); });
    menu.appendChild(item);
  };

  if (change.status === "Unmerged") {
    addItem("Resolve Conflict", () => openChangedFile(repoPath, change));
  } else {
    addItem("Show Diff", () => openChangedFile(repoPath, change));
    addItem("Rollback", () => rollbackChange(repoPath, change));
  }

  document.body.appendChild(menu);
  const dismiss = () => { menu.remove(); document.removeEventListener("click", dismiss); };
  setTimeout(() => document.addEventListener("click", dismiss), 0);
}

async function rollbackChange(repoPath: string, change: GitChange) {
  const ok = await appConfirm(`Rollback local changes to "${change.path}"? This cannot be undone.`);
  if (!ok) return;
  try {
    await gitDiscardChanges(repoPath, change.path, change.status);
    selectedFiles.delete(fileKey(repoPath, change.path));
    showStatus(`Rolled back: ${change.path}`);
    loadChanges(true);
  } catch (e) {
    showStatus(`Rollback failed: ${e}`, true);
  }
}

async function doCommit(andPush: boolean) {
  const msgEl = document.getElementById("changes-commit-msg") as HTMLTextAreaElement;
  const message = msgEl.value.trim();
  if (!message || selectedFiles.size === 0) return;

  const byRepo = new Map<string, string[]>();
  for (const key of selectedFiles) {
    const [repoPath, filePath] = key.split("\0");
    let list = byRepo.get(repoPath);
    if (!list) { list = []; byRepo.set(repoPath, list); }
    list.push(filePath);
  }

  const btnCommit = document.getElementById("changes-btn-commit") as HTMLButtonElement;
  const btnPush = document.getElementById("changes-btn-commit-push") as HTMLButtonElement;
  btnCommit.disabled = true;
  btnPush.disabled = true;

  const errors: string[] = [];
  for (const [repoPath, files] of byRepo) {
    try {
      showStatus(`Committing ${files.length} file(s) in ${repoPath.split("/").pop()}...`);
      await gitCommit(repoPath, files, message);
      if (andPush) {
        const branch = await gitCurrentBranch(repoPath);
        showStatus(`Pushing ${repoPath.split("/").pop()}...`);
        await gitPush(repoPath, branch);
      }
    } catch (e) {
      errors.push(`${repoPath.split("/").pop()}: ${e}`);
    }
  }

  if (errors.length > 0) {
    showStatus(errors.join("; "), true);
  } else {
    showStatus(andPush ? "Committed and pushed" : "Committed");
    msgEl.value = "";
  }
  selectedFiles.clear();
  updateCommitUI();
  loadChanges(true);
}

export function initChangesPanel(
  onOpenFile: (path: string) => void,
  onOpenDiff: (repoPath: string, change: GitChange, original: string, modified: string) => void,
) {
  openFileFn = onOpenFile;
  openDiffFn = onOpenDiff;
  changesContainer = document.getElementById("changes-list")!;
  changesContainer.addEventListener("scroll", scheduleVisibleRowsUpdate, { passive: true });
  changesContainer.addEventListener("click", handleChangesClick);
  changesContainer.addEventListener("contextmenu", handleChangesContextMenu);
  document.getElementById("changes-refresh")!.addEventListener("click", () => loadChanges(true));
  document.getElementById("changes-btn-commit")!.addEventListener("click", () => doCommit(false));
  document.getElementById("changes-btn-commit-push")!.addEventListener("click", () => doCommit(true));
  document.getElementById("changes-commit-msg")!.addEventListener("input", updateCommitUI);
}
