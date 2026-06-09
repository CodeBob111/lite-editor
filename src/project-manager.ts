import { EditorView } from "@codemirror/view";
import {
  app, currentProject, destroyAllCachedViews, destroyCachedViewsByPrefix,
  type ProjectState, type PersistedSession,
} from "./state";
import { showStatus, abbreviatePath } from "./utils";
import { createEditorState } from "./editor-setup";
import { rememberRecentProject, removeRecentProject } from "./recent-projects";
import { autoStartLsp, stopLspForProject, initJavaIndex, loadMavenModules } from "./lsp-manager";
import { isDiffTab, destroyActiveDiff } from "./diff-tabs";
import { onProjectChanged as astoreProjectChanged } from "./astore-panel";
import { resetDepAnalyzerState } from "./maven-helper";
import {
  readDirTree, readFile,
  startFileWatcher, stopFileWatcher,
  saveSessionData, loadSessionData,
} from "./tauri-api";
import type { TabManager } from "./tabs";
import type { FileTree } from "./file-tree";
import type { PanelManager } from "./panel";

let _tabManager: TabManager;
let _fileTree: FileTree;
let _panelManager: PanelManager;
let _editorContainer: HTMLElement;
let _onRefreshActivePanel: () => void = () => {};

export const welcomeContent = `// Welcome to Nib
//
// A lightweight code editor built with Tauri + CodeMirror 6
//
// Shortcuts:
//   Cmd+S         — Save file
//   Double Shift  — Quick Open File
//   Cmd+Shift+F   — Search in Files
//   Cmd+Click     — Find Usages (LSP)
//   F12           — Go to Definition (LSP)
//
// Right-click the file tree to create, rename, or delete files.
// Drag the borders between panels to resize them.
//
// Click "+" to open a project folder.
// Open multiple projects — they appear as tabs at the top.
`;

export interface ProjectManagerConfig {
  tabManager: TabManager;
  fileTree: FileTree;
  panelManager: PanelManager;
  editorContainer: HTMLElement;
  onRefreshActivePanel: () => void;
}

export function initProjectManager(config: ProjectManagerConfig) {
  _tabManager = config.tabManager;
  _fileTree = config.fileTree;
  _panelManager = config.panelManager;
  _editorContainer = config.editorContainer;
  _onRefreshActivePanel = config.onRefreshActivePanel;
}

export function syncActiveEditorToTab() {
  if (!app.currentFilePath || !app.editorView) return;
  _tabManager.setContent(app.currentFilePath, app.editorView.state.doc.toString());
}

function releaseCurrentEditor() {
  destroyActiveDiff();
  if (!app.editorView) return;
  syncActiveEditorToTab();
  if (app.currentFilePath && app.editorViewCache.get(app.currentFilePath) === app.editorView) {
    app.editorView.dom.remove();
  } else {
    app.editorView.destroy();
  }
  app.editorView = null;
}

function saveCurrentProjectState() {
  if (app.activeProjectIndex < 0 || app.activeProjectIndex >= app.projects.length) return;
  syncActiveEditorToTab();
  const project = app.projects[app.activeProjectIndex];
  const allTabs = _tabManager.getTabs();
  project.tabs = allTabs.filter((t) => !isDiffTab(t.path));
  const activeTab = allTabs[_tabManager.getActiveIndex()];
  if (activeTab && !isDiffTab(activeTab.path)) {
    project.activeTabIndex = project.tabs.findIndex((t) => t.path === activeTab.path);
    project.activeFilePath = activeTab.path;
  } else {
    project.activeTabIndex = Math.max(0, project.tabs.length - 1);
    project.activeFilePath = project.tabs[project.activeTabIndex]?.path ?? null;
  }
  project.treeRoot = _fileTree.getRoot();
}

export async function addProject(folderPath: string) {
  const existingIndex = app.projects.findIndex((p) => p.path === folderPath);
  if (existingIndex >= 0) {
    rememberRecentProject(folderPath);
    switchProject(existingIndex);
    return;
  }

  saveCurrentProjectState();

  const name = folderPath.split("/").pop() || folderPath;
  const newProject: ProjectState = {
    path: folderPath,
    name,
    treeRoot: null,
    tabs: [],
    activeTabIndex: -1,
    activeFilePath: null,
    allFilePathsCache: null,
  };

  app.projects.push(newProject);
  app.activeProjectIndex = app.projects.length - 1;
  app.currentProjectPath = folderPath;
  app.currentFilePath = null;

  _tabManager.closeAll();
  if (app.editorView) app.editorView.destroy();
  destroyAllCachedViews();
  app.editorView = null;

  showStatus(`Opening ${folderPath}...`);

  try {
    const tree = await readDirTree(folderPath);
    newProject.treeRoot = tree;
    _fileTree.setRoot(tree);
    astoreProjectChanged(folderPath);
    document.title = `${name} — Nib`;

    autoStartLsp(folderPath);
    loadMavenModules(folderPath);
    initJavaIndex(folderPath, true);
    startFileWatcher(folderPath).catch(() => {});
    rememberRecentProject(folderPath);

    showStatus(`Opened ${name}`);
  } catch (e) {
    removeRecentProject(folderPath);
    showStatus(`Failed to open folder: ${e}`, true);
  }

  if (!app.editorView) {
    const state = createEditorState(welcomeContent, "welcome.ts");
    app.editorView = new EditorView({ state, parent: _editorContainer });
  }

  renderProjectBar();
  _onRefreshActivePanel();
  debouncedSaveSession();
}

export function switchProject(index: number) {
  if (index === app.activeProjectIndex || index < 0 || index >= app.projects.length) return;

  const oldPath = app.currentProjectPath;

  saveCurrentProjectState();

  const project = app.projects[index];

  releaseCurrentEditor();
  resetDepAnalyzerState();
  if (oldPath) destroyCachedViewsByPrefix(oldPath);
  _editorContainer.innerHTML = "";

  app.activeProjectIndex = index;
  app.currentProjectPath = project.path;
  app.currentFilePath = null;

  _fileTree.setRoot(project.treeRoot ?? null);

  if (project.tabs.length > 0) {
    _tabManager.restoreState(project.tabs, project.activeTabIndex);
    app.currentFilePath = project.activeFilePath ?? _tabManager.getActiveFile();
  } else {
    _tabManager.closeAll();
    const state = createEditorState(welcomeContent, "welcome.ts");
    app.editorView = new EditorView({ state, parent: _editorContainer });
  }

  document.title = `${project.name} — Nib`;
  renderProjectBar();
  debouncedSaveSession();
  initJavaIndex(project.path);
  _onRefreshActivePanel();
  astoreProjectChanged(project.path);
  showStatus(`Switched to ${project.name}`);
}

export function closeProject(index: number) {
  if (index < 0 || index >= app.projects.length) return;

  const closedPath = app.projects[index].path;
  stopFileWatcher(closedPath).catch(() => {});
  stopLspForProject(closedPath);
  destroyCachedViewsByPrefix(closedPath);

  for (const key of app.diagnosticsMap.keys()) {
    const filePath = key.replace("file://", "");
    if (filePath.startsWith(closedPath)) {
      app.diagnosticsMap.delete(key);
    }
  }

  app.projects.splice(index, 1);

  if (app.editorView) app.editorView.destroy();
  destroyAllCachedViews();
  app.editorView = null;

  if (app.projects.length === 0) {
    app.activeProjectIndex = -1;
    app.currentProjectPath = null;
    app.currentFilePath = null;
    _tabManager.closeAll();
    _fileTree.setRoot(null);
    const state = createEditorState(welcomeContent, "welcome.ts");
    app.editorView = new EditorView({ state, parent: _editorContainer });
    document.title = "Nib";
  } else {
    if (index === app.activeProjectIndex) {
      app.activeProjectIndex = Math.min(index, app.projects.length - 1);
      const project = app.projects[app.activeProjectIndex];
      app.currentProjectPath = project.path;

      _fileTree.setRoot(project.treeRoot ?? null);

      if (project.tabs.length > 0) {
        _tabManager.restoreState(project.tabs, project.activeTabIndex);
        app.currentFilePath = project.activeFilePath ?? _tabManager.getActiveFile();
      } else {
        _tabManager.closeAll();
        app.currentFilePath = null;
        const state = createEditorState(welcomeContent, "welcome.ts");
        app.editorView = new EditorView({ state, parent: _editorContainer });
      }

      document.title = `${project.name} — Nib`;
    } else if (index < app.activeProjectIndex) {
      app.activeProjectIndex--;
    }
  }

  renderProjectBar();
  _onRefreshActivePanel();
  debouncedSaveSession();
}

export function renderProjectBar() {
  const bar = document.getElementById("project-bar-tabs")!;
  bar.innerHTML = "";

  app.projects.forEach((project, i) => {
    const tab = document.createElement("div");
    tab.className = `project-tab${i === app.activeProjectIndex ? " active" : ""}`;

    const shortPath = abbreviatePath(project.path);
    const parentDir = shortPath.substring(0, shortPath.lastIndexOf("/"));

    const closeSpan = document.createElement("span");
    closeSpan.className = "project-tab-close";
    closeSpan.textContent = "×";
    closeSpan.addEventListener("click", (e) => {
      e.stopPropagation();
      closeProject(i);
    });
    tab.appendChild(closeSpan);

    const nameSpan = document.createElement("span");
    nameSpan.className = "project-tab-name";
    nameSpan.textContent = project.name;
    tab.appendChild(nameSpan);

    if (parentDir) {
      const pathSpan = document.createElement("span");
      pathSpan.className = "project-tab-path";
      pathSpan.textContent = parentDir;
      tab.appendChild(pathSpan);
    }

    tab.addEventListener("click", () => switchProject(i));
    tab.addEventListener("auxclick", (e) => {
      if (e.button === 1) {
        e.preventDefault();
        closeProject(i);
      }
    });

    bar.appendChild(tab);
  });
}

// ---- Session persistence ----

export function debouncedSaveSession() {
  if (app.saveSessionTimeout) clearTimeout(app.saveSessionTimeout);
  app.saveSessionTimeout = setTimeout(saveSession, 250);
}

export function saveSession() {
  saveCurrentProjectState();

  const session: PersistedSession = {
    version: 1,
    projects: app.projects.map((p) => ({
      path: p.path,
      openFiles: p.tabs.map((t) => t.path).filter((p) => !isDiffTab(p)),
      activeFile: p.activeFilePath,
    })),
    activeProjectIndex: app.activeProjectIndex,
  };

  saveSessionData(JSON.stringify(session)).catch(() => {});
}

export async function loadSession() {
  try {
    const data = await loadSessionData();
    if (!data) {
      app.isRestoring = false;
      return;
    }

    const session: PersistedSession = JSON.parse(data);
    if (session.version !== 1 || !Array.isArray(session.projects)) {
      app.isRestoring = false;
      return;
    }

    const projectResults = await Promise.allSettled(
      session.projects.map(async (sp) => {
        const tree = await readDirTree(sp.path);
        const name = sp.path.split("/").pop() || sp.path;

        const project: ProjectState = {
          path: sp.path,
          name,
          treeRoot: tree,
          tabs: [],
          activeTabIndex: -1,
          activeFilePath: sp.activeFile,
          allFilePathsCache: null,
        };

        const fileResults = await Promise.allSettled(
          sp.openFiles.map(async (filePath) => {
            const content = await readFile(filePath);
            const fileName = filePath.split("/").pop() || filePath;
            return { path: filePath, name: fileName, content, dirty: false };
          }),
        );
        for (const r of fileResults) {
          if (r.status === "fulfilled") project.tabs.push(r.value);
        }

        if (sp.activeFile) {
          project.activeTabIndex = project.tabs.findIndex((t) => t.path === sp.activeFile);
          if (project.activeTabIndex < 0 && project.tabs.length > 0) {
            project.activeTabIndex = 0;
          }
        } else if (project.tabs.length > 0) {
          project.activeTabIndex = 0;
        }

        return { sp, project };
      }),
    );

    for (const r of projectResults) {
      if (r.status !== "fulfilled") continue;
      const { sp, project } = r.value;
      app.projects.push(project);
      rememberRecentProject(sp.path);

      autoStartLsp(sp.path);
      startFileWatcher(sp.path).catch(() => {});

      const deferredPath = sp.path;
      const deferJavaIndex = () => initJavaIndex(deferredPath, true);
      if ("requestIdleCallback" in window) (window as any).requestIdleCallback(deferJavaIndex);
      else setTimeout(deferJavaIndex, 100);
    }

    if (app.projects.length > 0) {
      app.activeProjectIndex = Math.min(
        Math.max(0, session.activeProjectIndex),
        app.projects.length - 1,
      );
      const project = app.projects[app.activeProjectIndex];
      app.currentProjectPath = project.path;

      if (project.treeRoot) {
        _fileTree.setRoot(project.treeRoot);
      }

      if (project.tabs.length > 0) {
        _tabManager.restoreState(project.tabs, project.activeTabIndex);
        app.currentFilePath = project.activeFilePath ?? _tabManager.getActiveFile();
      }

      document.title = `${project.name} — Nib`;
      loadMavenModules(project.path);
      renderProjectBar();
      showStatus(`Restored ${app.projects.length} project(s)`);

      if (!app.currentFilePath) {
        if (app.editorView) app.editorView.destroy();
        destroyAllCachedViews();
        app.editorView = null;
        const state = createEditorState(welcomeContent, "welcome.ts");
        app.editorView = new EditorView({ state, parent: _editorContainer });
      }
    }
  } catch {
    // invalid session data
  }

  app.isRestoring = false;
}

// ---- Tree refresh ----

function collectExpandedPaths(node: { path: string; isDir: boolean; expanded?: boolean; children?: any[] } | null, out: Set<string>) {
  if (!node) return;
  if (node.isDir && node.expanded) out.add(node.path);
  if (node.children) for (const c of node.children) collectExpandedPaths(c, out);
}

function applyExpandedPaths(node: { path: string; isDir: boolean; expanded?: boolean; children?: any[] }, paths: Set<string>) {
  if (node.isDir) node.expanded = paths.has(node.path);
  if (node.children) for (const c of node.children) applyExpandedPaths(c, paths);
}

let refreshInFlight = false;
let refreshPending = false;

// 合并重叠的刷新：单飞执行，期间到来的请求只置 pending，结束后至多补跑一次。
// 之前每次结构变更都各自发起一次 readDirTree——IPC 被堵时会堆叠十几个并发调用，
// 全部在通道疏通时一起返回（perf 报告里 refresh-tree 单次 max 477s 的成因）。
export async function refreshTree() {
  if (refreshInFlight) {
    refreshPending = true;
    return;
  }
  refreshInFlight = true;
  try {
    await doRefreshTree();
  } finally {
    refreshInFlight = false;
    if (refreshPending) {
      refreshPending = false;
      void refreshTree();
    }
  }
}

async function doRefreshTree() {
  if (!app.currentProjectPath) return;
  const project = currentProject();
  try {
    const expandedPaths = new Set<string>();
    if (project?.treeRoot) collectExpandedPaths(project.treeRoot, expandedPaths);
    // setRoot→rebuild 会把滚动位置重置到顶部,删除/新建后记下并还原,避免展开的子树滚出视野。
    const scrollTop = _fileTree.getScrollTop();

    const tree = await readDirTree(app.currentProjectPath, 4);
    requestAnimationFrame(() => {
      if (expandedPaths.size > 0) applyExpandedPaths(tree, expandedPaths);
      if (project) {
        project.treeRoot = tree;
        project.allFilePathsCache = null;
      }
      _fileTree.setRoot(tree);
      _fileTree.setScrollTop(scrollTop);
    });
  } catch {
    // ignore refresh failures
  }
}
