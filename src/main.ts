import { EditorView } from "@codemirror/view";
import { forceLinting } from "@codemirror/lint";

import { FileTree } from "./file-tree";
import { TabManager } from "./tabs";
import { PanelManager } from "./panel";
import {
  app, destroyCachedView,
} from "./state";
import { showStatus, getLanguageId } from "./utils";
import { initFileOps, openFile, navigateBack, navigateForward } from "./file-ops";
import { flashLine } from "./flash-line";
import {
  gotoDefinitionAtCursor, debouncedLspDidChange,
  hideUsagesPopup, usagesPopupNavigate, usagesPopupConfirm,
  setRevealDirectoryHandler,
} from "./lsp-navigation";
import { initGitPanel, loadGitBranches } from "./git-panel";
import { initChangesPanel, loadChanges, closeDiff } from "./changes-panel";
import { initMergeConflict } from "./merge-conflict";
import { initContextMenu, showContextMenu } from "./context-menu";
import { setupResizeHandles } from "./resize";
import { initMdPreview, toggleMdPreview, showPreviewButtonForFile, hideMdPreview, isMdPreviewActive, refreshMdPreview } from "./md-preview";
import { showSubTabsForFile, showDepAnalyzer, hideDepAnalyzer, isDepAnalyzerActive } from "./maven-helper";
import { initLongTaskObserver, formatReport, record } from "./perf-monitor";
import { initAstorePanel, onProjectChanged as astoreProjectChanged, toggleAstorePanel } from "./astore-panel";
import {
  openFolderDialog, lspDidOpen,
  onLspDiagnostics, onFileChanged, onMenuAction,
  writeFile, runMavenCommand, readFile,
  type FileChangeEvent,
} from "./tauri-api";

import { hydrateEditorLanguage } from "./editor-language";
import { isDiffTab, diffDataStore, destroyActiveDiff, renderDiffInEditor, openDiffAsTab, initDiffTabs } from "./diff-tabs";
import { initRecentProjects, showRecentProjects, hideRecentProjects } from "./recent-projects";
import { createEditorState, saveCurrentFile, initEditorSetup, applyExternalContent } from "./editor-setup";
import { ensureJavaLspForFile, initLspManager, initJavaIndex, loadMavenModules, lastMavenModules, isJavaIndexBuilding } from "./lsp-manager";
import {
  addProject, switchProject, closeProject, renderProjectBar,
  refreshTree, loadSession, saveSession, debouncedSaveSession,
  syncActiveEditorToTab, welcomeContent,
  initProjectManager,
} from "./project-manager";

// ---- Construct singletons ----

const editorContainer = document.getElementById("editor-container")!;

const tabManager = new TabManager(
  document.getElementById("tabs-bar")!,
  (filePath, content) => {
    const t0 = performance.now();

    if (isDiffTab(filePath)) {
      if (isDepAnalyzerActive()) hideDepAnalyzer();
      if (app.editorView) {
        const previousPath = app.currentFilePath;
        const previousView = app.editorView;
        if (previousPath && app.editorViewCache.get(previousPath) === previousView) {
          previousView.dom.remove();
        } else {
          syncActiveEditorToTab();
          previousView.destroy();
        }
        app.editorView = null;
      }
      app.currentFilePath = filePath;
      renderDiffInEditor(filePath);
      return;
    }

    destroyActiveDiff();

    if (app.currentFilePath === filePath && app.editorView) {
      fileTree.highlightFile(filePath);
      showSubTabsForFile(filePath);
      return;
    }

    if (app.editorView) {
      const previousPath = app.currentFilePath;
      const previousView = app.editorView;
      if (previousPath && app.editorViewCache.get(previousPath) === previousView) {
        previousView.dom.remove();
      } else {
        syncActiveEditorToTab();
        previousView.destroy();
      }
    }

    app.currentFilePath = filePath;

    const cached = app.editorViewCache.get(filePath);
    if (cached) {
      app.editorView = cached;
      editorContainer.appendChild(cached.dom);
      cached.requestMeasure();
      hydrateEditorLanguage(cached, filePath);
      record({ ts: Date.now(), kind: "ui", label: "tab-switch-cached", ms: performance.now() - t0, args: filePath });
      if (getLanguageId(filePath) === "java") {
        ensureJavaLspForFile(filePath).catch(() => {});
      }
    } else {
      const state = createEditorState(content, filePath);
      app.editorView = new EditorView({ state, parent: editorContainer });
      app.editorViewCache.set(filePath, app.editorView);
      hydrateEditorLanguage(app.editorView, filePath);
      record({ ts: Date.now(), kind: "ui", label: "tab-switch-new", ms: performance.now() - t0, args: filePath });
      const langId = getLanguageId(filePath);
      if (langId === "java") {
        ensureJavaLspForFile(filePath)
          .then((started) => {
            if (started) return lspDidOpen(filePath, langId, content);
          })
          .catch(() => {});
      } else {
        lspDidOpen(filePath, langId, content).catch(() => {});
      }
    }

    if (app.pendingScrollLine !== null) {
      const scrollTarget = app.pendingScrollLine;
      app.pendingScrollLine = null;
      const view = app.editorView;
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          const line = Math.min(scrollTarget, view.state.doc.lines);
          const tl = view.state.doc.line(line);
          view.dispatch({
            selection: { anchor: tl.from },
            effects: EditorView.scrollIntoView(tl.from, { y: "center" }),
          });
          flashLine(view, line);
        });
      });
    }

    fileTree.highlightFile(filePath);
    if (isMdPreviewActive()) hideMdPreview();
    showPreviewButtonForFile(filePath);
    showSubTabsForFile(filePath);
  },
  () => debouncedSaveSession(),
  (closedPath) => {
    if (isDiffTab(closedPath)) {
      diffDataStore.delete(closedPath);
      destroyActiveDiff();
    } else {
      destroyCachedView(closedPath);
    }
  },
);

const fileTree = new FileTree(
  document.getElementById("file-tree")!,
  (filePath) => openFile(filePath),
);

fileTree.setContextMenuHandler((node, x, y) => showContextMenu(node, x, y));
setRevealDirectoryHandler((path) => fileTree.revealFile(path));

const panelManager = new PanelManager(
  document.getElementById("panel-tabs")!,
  document.getElementById("panel-content")!,
);

// ---- Lazy modules ----

type SearchModule = typeof import("./search");
type TerminalPanelModule = typeof import("./terminal-panel");

let searchModulePromise: Promise<SearchModule> | null = null;
let searchInitialized = false;
let terminalPanelModulePromise: Promise<TerminalPanelModule> | null = null;
let terminalPanelModule: TerminalPanelModule | null = null;
let terminalPanelInitialized = false;

async function ensureSearchModule(): Promise<SearchModule> {
  if (!searchModulePromise) searchModulePromise = import("./search");
  const mod = await searchModulePromise;
  if (!searchInitialized) {
    mod.initSearch(fileTree);
    searchInitialized = true;
  }
  return mod;
}

async function ensureTerminalPanelModule(): Promise<TerminalPanelModule> {
  if (!terminalPanelModulePromise) terminalPanelModulePromise = import("./terminal-panel");
  const mod = await terminalPanelModulePromise;
  terminalPanelModule = mod;
  if (!terminalPanelInitialized) {
    mod.initTerminalPanel();
    terminalPanelInitialized = true;
  }
  return mod;
}

function openTerminalPanelLazy() {
  ensureTerminalPanelModule()
    .then((mod) => mod.openTerminalPanel())
    .catch((err) => showStatus(`Terminal failed to load: ${err}`, true));
}

function syncTerminalPanelProjectLazy() {
  if (terminalPanelModule) {
    terminalPanelModule.syncTerminalProject();
  }
}

// Wire the IDEA-style activity bar: clicking an icon switches the sidebar body
// between the Explorer and Commit views.
function initActivityBar() {
  const buttons = Array.from(document.querySelectorAll<HTMLElement>(".activity-btn"));
  const views = Array.from(document.querySelectorAll<HTMLElement>(".sidebar-view"));
  for (const btn of buttons) {
    btn.addEventListener("click", () => {
      const view = btn.dataset.view;
      buttons.forEach((b) => b.classList.toggle("active", b === btn));
      views.forEach((v) => v.classList.toggle("active", v.id === `${view}-view`));
      if (view === "commit") {
        // The view was display:none until the toggle above; defer one frame so
        // #changes-list has a real clientHeight before the virtualizer measures it.
        requestAnimationFrame(() => loadChanges());
      }
    });
  }
}

function refreshActivePanelForProject() {
  // The Commit view lives in the left sidebar now, independent of the bottom
  // panel, so refresh it whenever it's the active sidebar view.
  if (document.getElementById("commit-view")?.classList.contains("active")) {
    closeDiff();
    loadChanges(true);
  }
  switch (panelManager.getActivePanel()) {
    case "git":
      loadGitBranches();
      break;
    case "maven":
      if (app.currentProjectPath) loadMavenModules(app.currentProjectPath);
      break;
    case "terminal":
      syncTerminalPanelProjectLazy();
      openTerminalPanelLazy();
      break;
  }
}

// ---- Init all modules ----

initEditorSetup(tabManager);
initDiffTabs(editorContainer, tabManager);
initRecentProjects((path) => addProject(path));
initLspManager(panelManager);
initProjectManager({
  tabManager,
  fileTree,
  panelManager,
  editorContainer,
  onRefreshActivePanel: refreshActivePanelForProject,
});

initFileOps(tabManager);
initGitPanel();
initChangesPanel(
  (path) => openFile(path),
  (repoPath, change, original, modified) => openDiffAsTab(repoPath, change, original, modified),
);
initActivityBar();
panelManager.onSwitch("terminal", () => openTerminalPanelLazy());
initContextMenu(tabManager, refreshTree);
setupResizeHandles();
initMdPreview();
initMergeConflict();
initLongTaskObserver();
initAstorePanel(
  document.getElementById("astore-panel-content")!,
  document.getElementById("astore-msg-content")!,
);

// ---- Helper functions ----

function exportPerfReport() {
  const report = formatReport();
  const dest = (app.currentProjectPath || "/tmp") + "/perf-report.txt";
  writeFile(dest, report).then(() => {
    showStatus(`Perf report → ${dest}`);
  }).catch((err) => {
    showStatus(`Perf save failed: ${err}`, true);
  });
}

function openVcsCloneLazy() {
  import("./vcs-clone").then((mod) => {
    mod.showVcsClone((dir) => addProject(dir));
  }).catch((err) => showStatus(`VCS Clone failed to load: ${err}`, true));
}

function openSearchOverlayLazy() {
  ensureSearchModule()
    .then((mod) => mod.showSearchOverlay())
    .catch((err) => showStatus(`Search failed to load: ${err}`, true));
}

function openQuickOpenLazy() {
  ensureSearchModule()
    .then((mod) => mod.showQuickOpen())
    .catch((err) => showStatus(`Quick Open failed to load: ${err}`, true));
}

// ---- UI event listeners ----

document.getElementById("btn-md-preview")!.addEventListener("click", () => {
  if (app.currentFilePath?.endsWith(".md")) {
    const content = app.editorView?.state.doc.toString();
    toggleMdPreview(app.currentFilePath, content);
  }
});

document.getElementById("editor-sub-tabs")!.addEventListener("click", (e) => {
  const btn = (e.target as HTMLElement).closest<HTMLElement>(".editor-sub-tab");
  if (!btn) return;
  const sub = btn.dataset.sub;
  if (sub === "dep" && app.currentFilePath) {
    showDepAnalyzer(app.currentFilePath);
  } else if (sub === "text") {
    hideDepAnalyzer();
  }
});

document.getElementById("btn-locate-file")!.addEventListener("click", () => {
  const current = tabManager.getActiveFile();
  if (current) fileTree.revealFile(current);
});

document.getElementById("recent-projects-close")?.addEventListener("click", hideRecentProjects);
document.getElementById("recent-projects-overlay")?.addEventListener("click", (event) => {
  if (event.target === event.currentTarget) hideRecentProjects();
});

document.addEventListener("mousedown", (e) => {
  const popup = document.getElementById("usages-popup")!;
  if (!popup.classList.contains("hidden") && !popup.contains(e.target as Node)) {
    hideUsagesPopup();
  }
});

let lastLeftShiftDownAt = 0;
document.addEventListener("keydown", (e) => {
  if (e.defaultPrevented) return;

  const recentProjectsVisible = !document
    .getElementById("recent-projects-overlay")!
    .classList.contains("hidden");
  if (recentProjectsVisible && e.key === "Escape") {
    e.preventDefault();
    hideRecentProjects();
    return;
  }

  const vcsCloneVisible = !document
    .getElementById("vcs-clone-overlay")!
    .classList.contains("hidden");
  if (vcsCloneVisible && e.key === "Escape") {
    e.preventDefault();
    import("./vcs-clone").then((mod) => mod.hideVcsClone());
    return;
  }

  const usagesVisible = !document.getElementById("usages-popup")!.classList.contains("hidden");
  if (usagesVisible) {
    if (e.key === "ArrowDown") { e.preventDefault(); usagesPopupNavigate(1); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); usagesPopupNavigate(-1); return; }
    if (e.key === "Enter") { e.preventDefault(); usagesPopupConfirm(); return; }
    if (e.key === "Escape") { e.preventDefault(); hideUsagesPopup(); return; }
  }

  if ((e.metaKey || e.ctrlKey) && e.shiftKey && (e.key === "p" || e.key === "P")) {
    e.preventDefault();
    exportPerfReport();
    return;
  }

  if (e.key === "Meta" && app.editorView) {
    app.editorView.dom.classList.add("cmd-held");
  }

  if (!e.repeat && e.key === "Shift" && e.code === "ShiftLeft" && !e.metaKey && !e.ctrlKey && !e.altKey) {
    const now = performance.now();
    if (now - lastLeftShiftDownAt <= 420) {
      e.preventDefault();
      lastLeftShiftDownAt = 0;
      openQuickOpenLazy();
    } else {
      lastLeftShiftDownAt = now;
    }
  }
});

document.addEventListener("keyup", (e) => {
  if (e.key === "Meta" && app.editorView) {
    app.editorView.dom.classList.remove("cmd-held");
  }
});
window.addEventListener("blur", () => {
  if (app.editorView) app.editorView.dom.classList.remove("cmd-held");
});

// ---- Native menu actions ----

onMenuAction((id) => {
  switch (id) {
    case "open-folder":
      openFolderDialog().then((folder) => { if (folder) addProject(folder); });
      break;
    case "recent-projects":
      showRecentProjects();
      break;
    case "vcs-clone":
      openVcsCloneLazy();
      break;
    case "save":
      saveCurrentFile();
      break;
    case "close-tab":
      tabManager.closeActiveTab();
      break;
    case "find-in-files":
      openSearchOverlayLazy();
      break;
    case "quick-open":
      openQuickOpenLazy();
      break;
    case "goto-definition":
      if (app.editorView) gotoDefinitionAtCursor(app.editorView);
      break;
    case "navigate-back":
      navigateBack();
      break;
    case "navigate-forward":
      navigateForward();
      break;
    case "toggle-terminal":
      panelManager.switchTo("terminal");
      break;
    case "toggle-git":
      panelManager.switchTo("git");
      loadGitBranches();
      break;
    case "toggle-astore":
      toggleAstorePanel();
      break;
    case "export-perf":
      exportPerfReport();
      break;
  }
});

// ---- Maven toolbar ----

document.getElementById("maven-sync")!.addEventListener("click", async () => {
  if (!app.currentProjectPath) return;
  showStatus("Reloading Maven modules...");
  await loadMavenModules(app.currentProjectPath);
  showStatus("Maven modules reloaded");
});

document.getElementById("maven-generate")!.addEventListener("click", async () => {
  if (!app.currentProjectPath) return;
  panelManager.clearMavenOutput("=== mvn generate-sources ===");
  showStatus("Generating sources...");
  try {
    await runMavenCommand(app.currentProjectPath, ["generate-sources"]);
  } catch (err) {
    panelManager.appendMavenLine(`ERROR: ${err}`);
    showStatus("Generate sources failed", true);
  }
});

document.getElementById("maven-tree")!.addEventListener("click", async (e) => {
  const target = (e.target as HTMLElement).closest(".maven-action") as HTMLElement | null;
  if (!target) return;
  const cmd = target.dataset.cmd!;
  const mod = target.dataset.module!;
  const module = lastMavenModules.find((m) => m.artifact_id === mod);
  if (module && app.currentProjectPath) {
    const pomDir = module.pom_path.replace(/\/pom\.xml$/, "");
    panelManager.clearMavenOutput(`=== mvn ${cmd} (${mod}) ===`);
    showStatus(`Running mvn ${cmd} on ${mod}...`);
    try {
      await runMavenCommand(pomDir, [cmd]);
    } catch (err) {
      panelManager.appendMavenLine(`ERROR: ${err}`);
      showStatus(`mvn ${cmd} failed`, true);
    }
  }
});

// ---- LSP diagnostics ----

let lintRefreshTimer: ReturnType<typeof setTimeout> | null = null;
onLspDiagnostics((params) => {
  app.diagnosticsMap.set(params.uri, params.diagnostics);
  if (app.currentFilePath && params.uri === `file://${app.currentFilePath}` && app.editorView) {
    if (lintRefreshTimer === null) {
      lintRefreshTimer = setTimeout(() => {
        lintRefreshTimer = null;
        if (app.editorView) forceLinting(app.editorView);
      }, 300);
    }
  }
});

// ---- File watcher ----

let fileChangeDebounce: ReturnType<typeof setTimeout> | null = null;
let indexRebuildDebounce: ReturnType<typeof setTimeout> | null = null;
let changesRefreshDebounce: ReturnType<typeof setTimeout> | null = null;
let reloadCheckDebounce: ReturnType<typeof setTimeout> | null = null;

// Re-read every open file under `projectPath` from disk and, when it differs
// from what the editor is showing, reload it so external edits become visible.
// Path-agnostic on purpose: the watcher event's path is canonicalized
// (/private/... on macOS) and atomic saves rename through a temp file, so
// matching the event's path would miss real changes. Re-checking the bounded
// set of open tabs (LRU cap 30) is cheap and robust.
async function recheckOpenFilesAgainstDisk(projectPath: string | null) {
  if (!projectPath) return;
  for (const tab of tabManager.getTabs()) {
    if (isDiffTab(tab.path)) continue;
    if (!tab.path.startsWith(projectPath + "/")) continue;

    let disk: string;
    try {
      disk = await readFile(tab.path);
    } catch {
      continue; // deleted/unreadable — structural tree refresh handles removal
    }

    const view = app.editorViewCache.get(tab.path);
    const current = view ? view.state.doc.toString() : tab.content;
    if (disk === current) continue;                       // already in sync
    if (disk === app.savedContentCache.get(tab.path)) continue; // echo of our own save

    if (tab.dirty) {
      // External change collides with unsaved local edits — never clobber them.
      showStatus(`${tab.name} changed on disk — kept your unsaved edits`, true);
      continue;
    }

    if (view) applyExternalContent(view, disk);
    tabManager.setContent(tab.path, disk);
    debouncedLspDidChange(tab.path, () => disk);
    if (tab.path === app.currentFilePath) {
      if (isMdPreviewActive()) refreshMdPreview(disk);
      showStatus(`Reloaded ${tab.name} (changed on disk)`);
    }
  }
}

function scheduleReloadCheck(projectPath: string | null) {
  if (reloadCheckDebounce) clearTimeout(reloadCheckDebounce);
  reloadCheckDebounce = setTimeout(() => recheckOpenFilesAgainstDisk(projectPath), 200);
}

// Returning to the window catches changes the backend throttle may have dropped
// (mirrors how editors re-check on focus after you edit a file elsewhere).
window.addEventListener("focus", () => recheckOpenFilesAgainstDisk(app.currentProjectPath));

onFileChanged((evt: FileChangeEvent) => {
  if (evt.project === app.currentProjectPath) {
    scheduleReloadCheck(evt.project);

    if (evt.hasStructural) {
      if (fileChangeDebounce) clearTimeout(fileChangeDebounce);
      fileChangeDebounce = setTimeout(() => refreshTree(), 500);
    }

    const changesActive = document.getElementById("commit-view")?.classList.contains("active");
    if (changesActive) {
      if (changesRefreshDebounce) clearTimeout(changesRefreshDebounce);
      changesRefreshDebounce = setTimeout(() => loadChanges(), 3000);
    }

    if (app.javaIndexReady && !isJavaIndexBuilding(evt.project)) {
      if (indexRebuildDebounce) clearTimeout(indexRebuildDebounce);
      indexRebuildDebounce = setTimeout(async () => {
        try {
          await initJavaIndex(evt.project);
        } catch { /* ignore */ }
      }, 5000);
    }
  }
});

window.addEventListener("beforeunload", () => {
  saveSession();
});

// ---- Performance monitor (dev only) ----

if (import.meta.env.DEV) {
  const hud = document.createElement("div");
  hud.id = "perf-hud";
  hud.style.cssText = "position:fixed;top:4px;right:4px;z-index:99999;background:rgba(0,0,0,0.75);color:#0f0;font:11px/1.3 monospace;padding:4px 8px;border-radius:4px;pointer-events:none;white-space:pre";
  document.body.appendChild(hud);

  let frameTimes: number[] = [];
  let lastFrame = performance.now();
  let totalRealDrops = 0;
  let totalFrames = 0;
  const measureFrame = () => {
    const now = performance.now();
    const dt = now - lastFrame;
    frameTimes.push(dt);
    lastFrame = now;
    totalFrames++;
    if (dt > 33) totalRealDrops++;
    if (frameTimes.length >= 120) {
      frameTimes.sort((a, b) => a - b);
      const p50 = frameTimes[Math.floor(frameTimes.length * 0.5)];
      const p95 = frameTimes[Math.floor(frameTimes.length * 0.95)];
      const p99 = frameTimes[Math.floor(frameTimes.length * 0.99)];
      const jank = frameTimes.filter(t => t > 33).length;
      const color = jank > 3 ? "#f44" : jank > 0 ? "#fa0" : "#0f0";
      hud.style.color = color;
      hud.textContent = `p50=${p50.toFixed(1)} p95=${p95.toFixed(1)} p99=${p99.toFixed(1)}\njank=${jank}/120 total=${totalRealDrops}/${totalFrames}\ncache=${app.editorViewCache.size}`;
      const entry = `${new Date().toISOString()} p50=${p50.toFixed(1)} p95=${p95.toFixed(1)} p99=${p99.toFixed(1)} jank=${jank}/120 total=${totalRealDrops}/${totalFrames} cache=${app.editorViewCache.size}`;
      console.log(`[perf] ${entry}`);
      try {
        const log = JSON.parse(localStorage.getItem("perf-log") || "[]") as string[];
        log.push(entry);
        if (log.length > 500) log.splice(0, log.length - 500);
        localStorage.setItem("perf-log", JSON.stringify(log));
      } catch { /* quota exceeded */ }
      frameTimes = [];
    }
    requestAnimationFrame(measureFrame);
  };
  requestAnimationFrame(measureFrame);
}

// ---- Dev globals ----

if (import.meta.env.DEV) {
  (window as any).__app = app;
  (window as any).__createEditorState = createEditorState;
}

// ---- Startup ----

const state = createEditorState(welcomeContent, "welcome.ts");
app.editorView = new EditorView({ state, parent: editorContainer });

showStatus("Restoring session...");
loadSession().then(() => {
  if (app.projects.length === 0) {
    showStatus("Ready");
  }
  if (app.currentProjectPath) {
    astoreProjectChanged(app.currentProjectPath);
  }
});
