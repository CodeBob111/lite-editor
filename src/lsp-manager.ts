import { app } from "./state";
import { showStatus } from "./utils";
import {
  startLsp,
  stopLsp,
  onLspProgress,
  parseMavenModules,
  onMavenOutput,
  onMavenDone,
  buildJavaIndex,
  loadJavaIndex,
  buildUsageIndex,
  gitDiscoverRepos,
  type MavenModule,
} from "./tauri-api";
import type { PanelManager } from "./panel";

let _panelManager: PanelManager;
export let lastMavenModules: MavenModule[] = [];

export function initLspManager(panelManager: PanelManager) {
  _panelManager = panelManager;

  onLspProgress((event) => {
    const container = document.getElementById("lsp-progress")!;
    const textEl = document.getElementById("lsp-progress-text")!;
    const fillEl = document.getElementById("lsp-progress-fill")!;

    if (lspProgressStallTimer) clearTimeout(lspProgressStallTimer);

    if (event.kind === "end") {
      container.classList.add("hidden");
      showStatus("LSP indexing complete");
      lastProgressUpdate = 0;
      return;
    }

    const now = performance.now();
    if (now - lastProgressUpdate < 500) return;
    lastProgressUpdate = now;

    container.classList.remove("hidden");
    const label = event.message || "Indexing...";
    const pct = event.percentage != null ? `${event.percentage}%` : "";
    textEl.textContent = pct ? `${label} ${pct}` : label;
    fillEl.style.width = event.percentage != null ? `${event.percentage}%` : "100%";

    lspProgressStallTimer = setTimeout(() => {
      container.classList.add("hidden");
      showStatus("LSP indexing may have stalled — Cmd+Click re-enabled");
    }, 60_000);
  });

  onMavenOutput((line) => {
    _panelManager.appendMavenLine(line);
  });
  onMavenDone((exitCode) => {
    if (exitCode === 0) {
      _panelManager.appendMavenLine("\n=== BUILD SUCCESS ===");
      showStatus("Maven build completed");
    } else {
      _panelManager.appendMavenLine(`\n=== BUILD FAILED (exit ${exitCode}) ===`);
      showStatus("Maven build failed", true);
    }
  });
}

let lspProgressStallTimer: ReturnType<typeof setTimeout> | null = null;
let lastProgressUpdate = 0;

export function detectProjectLanguages(rootPath: string): Set<string> {
  const project = app.projects.find((p) => p.path === rootPath);
  const children = project?.treeRoot?.children;
  if (!children) return new Set();

  const markers: Record<string, string[]> = {
    python: ["requirements.txt", "pyproject.toml", "setup.py", "setup.cfg", "Pipfile"],
    typescript: ["package.json", "tsconfig.json", "deno.json"],
  };

  const detected = new Set<string>();
  for (const child of children) {
    if (child.isDir) continue;
    for (const [lang, files] of Object.entries(markers)) {
      if (files.includes(child.name)) detected.add(lang);
    }
  }
  return detected;
}

export function autoStartLsp(rootPath: string) {
  const langs = detectProjectLanguages(rootPath);
  const promises: Promise<void>[] = [];
  if (langs.has("python")) promises.push(startLsp("python", rootPath).catch(() => {}));
  if (langs.has("typescript")) promises.push(startLsp("typescript", rootPath).catch(() => {}));
  if (promises.length) Promise.all(promises);
}

const javaLspStartPromises = new Map<string, Promise<void>>();
const javaLspRootsByProject = new Map<string, Set<string>>();
const repoRootCache = new Map<string, Promise<string[]>>();

// 常驻 jdtls 实例的 LRU 上限:每个 git 仓库各起一个 jdtls(各占约 1.5G 堆),
// 像 rate-native 这种装了十来个仓库的工作区,挨个开文件会攒出十来个常驻 jdtls 把内存吃光。
// 这里限制最多常驻 MAX_WARM_JAVA_LSP 个,超了就关掉最久没用的(被关的仓库再打开会重启,
// 但工作区已落盘,比首次导入快)。
const MAX_WARM_JAVA_LSP = 3;
const javaLspLru: string[] = []; // 最近使用在前

function touchJavaLspRoot(rootPath: string) {
  const i = javaLspLru.indexOf(rootPath);
  if (i >= 0) javaLspLru.splice(i, 1);
  javaLspLru.unshift(rootPath);
}

function forgetJavaLspRoot(rootPath: string) {
  const i = javaLspLru.indexOf(rootPath);
  if (i >= 0) javaLspLru.splice(i, 1);
  javaLspStartPromises.delete(rootPath);
  for (const roots of javaLspRootsByProject.values()) roots.delete(rootPath);
}

// 超过上限时,关掉最久未用的 jdtls(keepRoot 刚用过、在最前,不会被选中)。
function evictExcessJavaLsp() {
  while (javaLspLru.length > MAX_WARM_JAVA_LSP) {
    const victim = javaLspLru.pop()!;
    javaLspStartPromises.delete(victim);
    for (const roots of javaLspRootsByProject.values()) roots.delete(victim);
    stopLsp("java", victim).catch(() => {});
  }
}

function deepestMatchingRoot(filePath: string, roots: string[]) {
  return roots
    .filter((root) => filePath === root || filePath.startsWith(root + "/"))
    .sort((a, b) => b.length - a.length)[0] ?? null;
}

async function repoRootsForProject(projectPath: string) {
  let promise = repoRootCache.get(projectPath);
  if (!promise) {
    promise = gitDiscoverRepos(projectPath)
      .then((repos) => repos.map((repo) => repo.path))
      .catch(() => []);
    repoRootCache.set(projectPath, promise);
  }
  return promise;
}

async function javaLspRootForFile(filePath: string) {
  const projectPath = app.currentProjectPath;
  if (!projectPath || (filePath !== projectPath && !filePath.startsWith(projectPath + "/"))) {
    return null;
  }

  const repoRoot = deepestMatchingRoot(filePath, await repoRootsForProject(projectPath));
  if (repoRoot) return repoRoot;

  const mavenRoots = lastMavenModules.map((module) => module.pom_path.replace(/\/pom\.xml$/, ""));
  return deepestMatchingRoot(filePath, mavenRoots) ?? projectPath;
}

export async function ensureJavaLspForFile(filePath: string) {
  const rootPath = await javaLspRootForFile(filePath);
  if (!rootPath) return false;

  touchJavaLspRoot(rootPath); // 标记最近使用,避免刚用的被 LRU 淘汰

  let promise = javaLspStartPromises.get(rootPath);
  if (!promise) {
    promise = startLsp("java", rootPath).catch((err) => {
      forgetJavaLspRoot(rootPath);
      throw err;
    });
    javaLspStartPromises.set(rootPath, promise);
    evictExcessJavaLsp(); // 新增了一个常驻实例 → 超限就关掉最久没用的
  }
  await promise;

  const projectPath = app.currentProjectPath;
  if (projectPath) {
    let roots = javaLspRootsByProject.get(projectPath);
    if (!roots) {
      roots = new Set();
      javaLspRootsByProject.set(projectPath, roots);
    }
    roots.add(rootPath);
  }
  return true;
}

export function stopLspForProject(projectPath: string) {
  stopLsp("python", projectPath).catch(() => {});
  stopLsp("typescript", projectPath).catch(() => {});
  stopLsp("java", projectPath).catch(() => {});

  const roots = javaLspRootsByProject.get(projectPath);
  if (roots) {
    for (const root of roots) {
      javaLspStartPromises.delete(root);
      const i = javaLspLru.indexOf(root);
      if (i >= 0) javaLspLru.splice(i, 1);
      stopLsp("java", root).catch(() => {});
    }
    javaLspRootsByProject.delete(projectPath);
  }
}

const javaIndexBuildInFlight = new Map<string, Promise<void>>();
const javaIndexLastBuilt = new Map<string, number>();
const JAVA_INDEX_COOLDOWN = 30_000;

export async function initJavaIndex(projectPath: string, force = false) {
  if (javaIndexBuildInFlight.has(projectPath)) return;
  if (!force) {
    const last = javaIndexLastBuilt.get(projectPath) ?? 0;
    if (performance.now() - last < JAVA_INDEX_COOLDOWN) return;
  }
  const p = (async () => {
    try {
      const cached = await loadJavaIndex(projectPath);
      if (cached > 0) {
        app.javaIndexReady = true;
        showStatus(`Java index loaded (${cached} classes)`);
      }
      const fresh = await buildJavaIndex(projectPath);
      app.javaIndexReady = true;
      javaIndexLastBuilt.set(projectPath, performance.now());
      if (cached === 0) showStatus(`Java index built (${fresh} classes)`);
      // 顺带构建「符号出现」倒排索引(供 find-usages 瞬时查询,不依赖 jdtls)。
      // 增量 + 落盘缓存,后台跑,不阻塞 Java 类索引。
      buildUsageIndex(projectPath).catch(() => {});
    } catch { /* ignore */ }
  })();
  javaIndexBuildInFlight.set(projectPath, p);
  await p;
  javaIndexBuildInFlight.delete(projectPath);
}

export function isJavaIndexBuilding(projectPath: string): boolean {
  return javaIndexBuildInFlight.has(projectPath);
}

export async function loadMavenModules(projectPath: string) {
  const tree = document.getElementById("maven-tree");
  if (tree) tree.innerHTML = '<div class="panel-placeholder">Loading Maven modules...</div>';
  lastMavenModules = [];
  try {
    const modules = await parseMavenModules(projectPath);
    if (app.currentProjectPath !== projectPath) return;
    lastMavenModules = modules;
    _panelManager.showMavenModules(
      modules.map((m) => ({
        name: m.name,
        groupId: m.group_id,
        artifactId: m.artifact_id,
      })),
    );
  } catch {
    if (app.currentProjectPath === projectPath && tree) {
      tree.innerHTML = '<div class="panel-placeholder">No Maven modules found</div>';
    }
  }
}
