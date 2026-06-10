// refreshTree 的回归测试:切项目竞态防回灌。
// 真 bug:doRefreshTree 在开头捕获 project,await readDirTree(大仓库可能数秒)期间
// 用户切到另一个项目,返回后无条件 _fileTree.setRoot(tree),把已切走的旧项目树盖回
// 可见文件树——表现为"打开 A 却显示 B 的目录"。修复后:仍回写旧项目自己的 treeRoot 缓存,
// 但只有它仍是当前项目时才回灌可见树。
import { describe, it, expect, vi, beforeEach } from "vitest";

// doRefreshTree 只依赖 state(真) + readDirTree(受控) + 注入的 _fileTree + rAF。
// 其余 sibling 依赖与本用例无关,全部 mock 成空操作,保证用例 hermetic、不触 DOM。
vi.mock("@codemirror/view", () => ({ EditorView: class {} }));
vi.mock("./utils", () => ({ showStatus: vi.fn(), abbreviatePath: (p: string) => p }));
vi.mock("./editor-setup", () => ({ createEditorState: vi.fn() }));
vi.mock("./recent-projects", () => ({ rememberRecentProject: vi.fn(), removeRecentProject: vi.fn() }));
vi.mock("./lsp-manager", () => ({
  autoStartLsp: vi.fn(), stopLspForProject: vi.fn(), initJavaIndex: vi.fn(), loadMavenModules: vi.fn(),
}));
vi.mock("./diff-tabs", () => ({ isDiffTab: () => false, destroyActiveDiff: vi.fn() }));
vi.mock("./astore-panel", () => ({ onProjectChanged: vi.fn() }));
vi.mock("./maven-helper", () => ({ resetDepAnalyzerState: vi.fn() }));
vi.mock("./tauri-api", () => ({
  readDirTree: vi.fn(),
  readFile: vi.fn(),
  startFileWatcher: vi.fn(() => Promise.resolve()),
  stopFileWatcher: vi.fn(() => Promise.resolve()),
  saveSessionData: vi.fn(() => Promise.resolve()),
  loadSessionData: vi.fn(() => Promise.resolve(null)),
}));

import { app, type ProjectState } from "./state";
import { initProjectManager, refreshTree } from "./project-manager";
import { readDirTree } from "./tauri-api";

function project(path: string): ProjectState {
  return { path, name: path.slice(1), treeRoot: null, tabs: [], activeTabIndex: -1, activeFilePath: null, allFilePathsCache: null };
}
const node = (path: string) => ({ path, name: path.slice(1), isDir: true, expanded: true, children: [] });

let setRoot: ReturnType<typeof vi.fn>;

beforeEach(() => {
  // rAF 同步执行,让 doRefreshTree 的回灌在 await refreshTree() 返回前完成,断言无需等帧。
  (globalThis as any).requestAnimationFrame = (cb: (t: number) => void) => { cb(0); return 0; };
  setRoot = vi.fn();
  initProjectManager({
    tabManager: {} as any,
    fileTree: { getScrollTop: () => 0, setRoot, setScrollTop: vi.fn() } as any,
    panelManager: {} as any,
    editorContainer: {} as any,
    onRefreshActivePanel: () => {},
  });
  app.projects.length = 0;
  app.projects.push(project("/A"), project("/B"));
  app.activeProjectIndex = 0;
  app.currentProjectPath = "/A";
  vi.mocked(readDirTree).mockReset();
});

describe("refreshTree 切项目竞态", () => {
  it("await 期间切走:不把旧项目的树回灌到可见文件树", async () => {
    const treeA = node("/A");
    // 模拟:readDirTree 解析前用户切到了 /B(currentProjectPath 改变)。
    vi.mocked(readDirTree).mockImplementation(async () => {
      app.currentProjectPath = "/B";
      app.activeProjectIndex = 1;
      return treeA as any;
    });

    await refreshTree();

    // 可见树不能被旧项目 /A 的树覆盖。
    expect(setRoot).not.toHaveBeenCalled();
    // 但 /A 自己的 treeRoot 缓存仍应更新——切回 /A 时可直接复用。
    expect(app.projects[0].treeRoot).toBe(treeA);
  });

  it("未切走:正常把当前项目的树回灌到可见文件树", async () => {
    const treeA = node("/A");
    vi.mocked(readDirTree).mockResolvedValue(treeA as any);

    await refreshTree();

    expect(setRoot).toHaveBeenCalledWith(treeA);
    expect(app.projects[0].treeRoot).toBe(treeA);
  });
});
