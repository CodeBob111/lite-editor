import { EditorView } from "@codemirror/view";
import { forceLinting } from "@codemirror/lint";

import { FileTree } from "./file-tree";
import { TabManager } from "./tabs";
import { PanelManager } from "./panel";
import {
  app, destroyCachedView,
} from "./state";
import { showStatus, getLanguageId } from "./utils";
import { updateStatusBar } from "./status-bar";
import { initSettings } from "./settings";
import { openSettings, closeSettings, isSettingsOpen } from "./settings-ui";
import { initWelcomeScreen, refreshWelcomeScreen } from "./welcome-screen";
import { initFileOps, openFile, navigateBack, navigateForward, navRecordTabSwitch } from "./file-ops";
import { flashLine } from "./flash-line";
import {
  gotoDefinitionAtCursor, debouncedLspDidChange,
  hideUsagesPopup, usagesPopupNavigate, usagesPopupConfirm,
  setRevealDirectoryHandler,
} from "./lsp-navigation";
import { initGitPanel, loadGitBranches } from "./git-panel";
import { initChangesPanel, loadChanges, closeDiff } from "./changes-panel";
import { initMergeConflict } from "./merge-conflict";
import { initContextMenu, showContextMenu, showMultiContextMenu } from "./context-menu";
import { setupResizeHandles } from "./resize";
import { initMdPreview, toggleMdPreview, showPreviewButtonForFile, hideMdPreview, isMdPreviewActive, refreshMdPreview } from "./md-preview";
import { showSubTabsForFile, showDepAnalyzer, hideDepAnalyzer, isDepAnalyzerActive } from "./maven-helper";
import { initAstorePanel, onProjectChanged as astoreProjectChanged, toggleAstorePanel } from "./astore-panel";
import {
  openFolderDialog, lspDidOpen,
  onLspDiagnostics, onFileChanged, onMenuAction,
  runMavenCommand, readFile,
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

// 记住每个文件的滚动位置:CodeMirror 视图从 DOM 摘下再挂回会丢 scrollTop、requestMeasure 也不还原,
// 导致切走再切回跳回顶部。摘下时存、挂回后还原。
const editorScrollByPath = new Map<string, number>();

const tabManager = new TabManager(
  document.getElementById("tabs-bar")!,
  (filePath, content, reason) => {
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

    // 记录导航历史(cmd+[ / cmd+]):离开当前文件前先抓住它的最新光标行。
    const navPrevFile = app.currentFilePath;
    const navPrevLine = (navPrevFile && app.editorView)
      ? app.editorView.state.doc.lineAt(app.editorView.state.selection.main.head).number
      : 1;

    if (app.currentFilePath === filePath && app.editorView) {
      fileTree.highlightFile(filePath);
      showSubTabsForFile(filePath);
      return;
    }

    if (app.editorView) {
      const previousPath = app.currentFilePath;
      const previousView = app.editorView;
      if (previousPath && app.editorViewCache.get(previousPath) === previousView) {
        editorScrollByPath.set(previousPath, previousView.scrollDOM.scrollTop);
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
      // 还原切走前记住的滚动位置(摘挂 DOM 会清零 scrollTop)。延后一帧、等测量完再设;
      // 若是「跳转到指定行」(pendingScrollLine)则让位给跳转,不还原。
      const savedScroll = editorScrollByPath.get(filePath);
      if (savedScroll != null && app.pendingScrollLine === null) {
        requestAnimationFrame(() => {
          if (app.editorView === cached) cached.scrollDOM.scrollTop = savedScroll;
        });
      }
      if (getLanguageId(filePath) === "java") {
        ensureJavaLspForFile(filePath).catch(() => {});
      }
    } else {
      const state = createEditorState(content, filePath);
      app.editorView = new EditorView({ state, parent: editorContainer });
      app.editorViewCache.set(filePath, app.editorView);
      hydrateEditorLanguage(app.editorView, filePath);
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
    updateStatusBar();
    refreshWelcomeScreen();

    // 仅用户主动的打开/切换记入历史(关闭后激活相邻页、会话恢复不记)。
    // openFileAtLine 驱动的激活由 suppressTabNavRecord 抑制,避免重复记录。
    if (reason === "open" || reason === "switch") {
      const navNewLine = app.editorView
        ? app.editorView.state.doc.lineAt(app.editorView.state.selection.main.head).number
        : 1;
      // diff 标签页不是真实文件,不能作为「后退」目标(否则 readFile("diff:...") 会失败)。
      const prevFile = navPrevFile && !isDiffTab(navPrevFile) ? navPrevFile : null;
      navRecordTabSwitch(prevFile, navPrevLine, filePath, navNewLine);
    }
  },
  () => debouncedSaveSession(),
  (closedPath) => {
    if (isDiffTab(closedPath)) {
      diffDataStore.delete(closedPath);
      destroyActiveDiff();
    } else {
      destroyCachedView(closedPath);
      editorScrollByPath.delete(closedPath);
    }
  },
  () => {
    // 最后一个标签关闭 → 回到空状态:销毁残留的编辑器视图、清空当前文件,
    // 并收起 Markdown 预览与子标签(否则关掉所有 md 标签后,右侧预览框还挂着)。
    if (app.editorView) {
      app.editorView.dom.remove();
      app.editorView.destroy();
      app.editorView = null;
    }
    app.currentFilePath = null;
    showPreviewButtonForFile(null);
    showSubTabsForFile(null);
    fileTree.highlightFile("");
    updateStatusBar();
    refreshWelcomeScreen();
  },
);

const fileTree = new FileTree(
  document.getElementById("file-tree")!,
  (filePath) => openFile(filePath),
);

fileTree.setContextMenuHandler((node, x, y) => showContextMenu(node, x, y));
fileTree.setMultiContextMenuHandler((nodes, x, y) => showMultiContextMenu(nodes, x, y));
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
// 切换左侧活动栏视图:Explorer / Commit / Git / Maven。按钮与视图都用 data-view 配对
// (Git/Maven 已从底部面板移到左侧,故不再用 `${view}-view` 的 id 约定)。
function activateSidebarView(view: string) {
  document.querySelectorAll<HTMLElement>(".activity-btn[data-view]").forEach((b) =>
    b.classList.toggle("active", b.dataset.view === view));
  document.querySelectorAll<HTMLElement>(".sidebar-view").forEach((v) =>
    v.classList.toggle("active", v.dataset.view === view));
  if (view === "commit") {
    // The view was display:none until the toggle above; defer one frame so
    // #changes-list has a real clientHeight before the virtualizer measures it.
    requestAnimationFrame(() => loadChanges());
  } else if (view === "git") {
    loadGitBranches();
  } else if (view === "maven") {
    if (app.currentProjectPath) loadMavenModules(app.currentProjectPath);
  }
}

function initActivityBar() {
  // 只有带 data-view 的按钮参与切换;底部齿轮是独立动作(打开设置)。
  document.querySelectorAll<HTMLElement>(".activity-btn[data-view]").forEach((btn) => {
    btn.addEventListener("click", () => activateSidebarView(btn.dataset.view!));
  });
  document.getElementById("btn-settings")?.addEventListener("click", () => openSettings());
  // Maven 构建开始时,自动聚焦左侧 Maven 视图(取代原来切到底部 Maven tab)。
  panelManager.setMavenFocusHandler(() => activateSidebarView("maven"));
}

function refreshActivePanelForProject() {
  // Commit / Git / Maven 现在都是左侧栏视图:刷新当前激活的那个。
  const activeView = document.querySelector<HTMLElement>(".sidebar-view.active")?.dataset.view;
  if (activeView === "commit") {
    closeDiff();
    loadChanges(true);
  } else if (activeView === "git") {
    loadGitBranches();
  } else if (activeView === "maven") {
    if (app.currentProjectPath) loadMavenModules(app.currentProjectPath);
  }
  // 底部面板现在只剩 Terminal / Astore Message。
  if (panelManager.getActivePanel() === "terminal") {
    syncTerminalPanelProjectLazy();
    openTerminalPanelLazy();
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
initWelcomeScreen({
  onOpenFolder: () => openFolderDialog().then((folder) => { if (folder) addProject(folder); }),
  onClone: () => import("./vcs-clone").then((mod) => mod.showVcsClone((dir) => addProject(dir))),
  onNewTerminal: () => openTerminalPanelLazy(),
  onOpenRecent: (path) => addProject(path),
});
panelManager.onSwitch("terminal", () => openTerminalPanelLazy());
initContextMenu(tabManager, refreshTree);
setupResizeHandles();
initMdPreview();
initMergeConflict();
initAstorePanel(
  document.getElementById("astore-panel-content")!,
  document.getElementById("astore-msg-content")!,
);

// ---- Helper functions ----

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

  // Cmd/Ctrl+C 兜底复制:WKWebView 对普通 DOM 选区(Markdown 预览、Git 分支、文件树、
  // 状态栏等)的原生复制不可靠 —— 这些区域只要鼠标能选中,就把选区文本写入剪贴板。
  // CodeMirror 与 input/textarea 自带复制,这里不拦截,交给原生。
  if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === "c" || e.key === "C")) {
    const active = document.activeElement as HTMLElement | null;
    const selfHandled = !!active &&
      (active.tagName === "INPUT" || active.tagName === "TEXTAREA" || !!active.closest(".cm-editor"));
    if (!selfHandled) {
      const text = window.getSelection()?.toString() ?? "";
      if (text) {
        e.preventDefault();
        navigator.clipboard.writeText(text).catch(() => {});
      }
    }
    // selfHandled 或无选区:不拦截,落到原生复制。
  }

  // 设置屏:Cmd+, 开;开着时吞掉 Esc 关闭,并屏蔽其它应用快捷键(输入键照常进控件)。
  if ((e.metaKey || e.ctrlKey) && e.key === ",") {
    e.preventDefault();
    openSettings();
    return;
  }
  if (isSettingsOpen()) {
    if (e.key === "Escape") { e.preventDefault(); closeSettings(); }
    return;
  }

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
      activateSidebarView("git");
      break;
    case "toggle-astore":
      toggleAstorePanel();
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

// ---- Startup ----

const state = createEditorState(welcomeContent, "welcome.ts");
app.editorView = new EditorView({ state, parent: editorContainer });
refreshWelcomeScreen(); // 启动先显示欢迎屏;恢复会话打开文件后 onTabActivate 会隐藏它。

showStatus("Restoring session...");
// 先载入偏好设置(应用字体 CSS 变量 + 让随后恢复的文件按当前 tabSize/wrap 等初始化),
// 再恢复会话。
initSettings().finally(() => {
  loadSession().then(() => {
    if (app.projects.length === 0) {
      showStatus("Ready");
    }
    if (app.currentProjectPath) {
      astoreProjectChanged(app.currentProjectPath);
    }
    // Terminal 现在是底部面板的默认激活项,但「点已激活的 tab」会被 panelManager.switchTo
    // 的 early-return 吞掉、onSwitch 不触发,且 loadSession 只硬编码加载了 Maven。
    // 所以启动时在这里主动初始化并打开终端:接线 + 按钮,并按当前项目开一个终端。
    if (panelManager.getActivePanel() === "terminal") openTerminalPanelLazy();
    refreshWelcomeScreen(); // 会话恢复后再校正一次(无文件则保持显示)。
  });
});
