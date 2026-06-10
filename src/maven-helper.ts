import {
  mavenDependencyTree, mavenAddExclusion,
  type DepNode, type MavenConflict, type MavenConflictNode, type MavenFlatDep, type DepCoordRef,
} from "./tauri-api";
import { app } from "./state";
import { showStatus } from "./utils";

// 解析/冲突检测/扁平化已迁入 Rust(src-tauri/src/maven.rs),本文件只剩视图状态与渲染。

// ---- State ----

let currentPomPath: string | null = null;
let currentTree: DepNode | null = null;
let currentConflicts: MavenConflict[] = [];
let currentFlat: MavenFlatDep[] = [];
let currentView: "conflicts" | "list" | "tree" = "list";
let searchQuery = "";
let showGroupId = false;
let hideTestScope = false;
let isLoading = false;
let showSize = false;

// ---- Exclude dependency (pom.xml 编辑在 Rust 侧完成) ----

async function excludeDependency(
  pomPath: string,
  parentGroupId: string, parentArtifactId: string,
  excludeGroupId: string, excludeArtifactId: string,
): Promise<void> {
  try {
    await mavenAddExclusion(pomPath, parentGroupId, parentArtifactId, excludeGroupId, excludeArtifactId);
    showStatus(`Excluded ${excludeGroupId}:${excludeArtifactId}`);
  } catch (err) {
    showStatus(`${err}`, true);
  }
}

// ---- UI Rendering ----

function getContainer(): HTMLElement {
  return document.getElementById("dep-analyzer-content")!;
}

function setPlaceholder(container: HTMLElement, text: string, isError = false) {
  const div = document.createElement("div");
  div.className = `dep-placeholder${isError ? " dep-error" : ""}`;
  div.textContent = text;
  container.replaceChildren(div);
}

function renderToolbar() {
  const toolbar = document.getElementById("dep-analyzer-toolbar")!;
  toolbar.replaceChildren();

  // Row 1: Refresh + Reimport buttons
  const row1 = document.createElement("div");
  row1.className = "dep-toolbar-row";
  const refreshBtn = document.createElement("button");
  refreshBtn.className = "dep-btn";
  refreshBtn.textContent = "Refresh UI";
  refreshBtn.title = "Re-run mvn dependency:tree";
  refreshBtn.addEventListener("click", () => refreshDependencyTree());
  const reimportBtn = document.createElement("button");
  reimportBtn.className = "dep-btn";
  reimportBtn.textContent = "Reimport";
  reimportBtn.title = "Force reimport dependencies";
  reimportBtn.addEventListener("click", () => refreshDependencyTree());
  row1.append(refreshBtn, reimportBtn);
  toolbar.appendChild(row1);

  // Row 2: Conflicts radio + search box
  const row2 = document.createElement("div");
  row2.className = "dep-toolbar-row";
  const conflictsRadio = createRadio("Conflicts", "conflicts");
  row2.appendChild(conflictsRadio);
  const searchBox = document.createElement("input");
  searchBox.type = "text";
  searchBox.className = "dep-search";
  searchBox.placeholder = "Search...";
  searchBox.value = searchQuery;
  searchBox.addEventListener("input", () => { searchQuery = searchBox.value; renderContent(); });
  row2.appendChild(searchBox);
  toolbar.appendChild(row2);

  // Row 3: All Dependencies as List + checkboxes
  const row3 = document.createElement("div");
  row3.className = "dep-toolbar-row";
  row3.appendChild(createRadio("All Dependencies as List", "list"));
  const opts: [string, boolean, (v: boolean) => void][] = [
    ["Show GroupId", showGroupId, (v) => { showGroupId = v; renderContent(); }],
    ["Show Size", showSize, (v) => { showSize = v; renderContent(); }],
    ["Hide Test Scope", hideTestScope, (v) => { hideTestScope = v; renderContent(); }],
  ];
  for (const [label, checked, onChange] of opts) {
    const lbl = document.createElement("label");
    lbl.className = "dep-checkbox";
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = checked;
    cb.addEventListener("change", () => onChange(cb.checked));
    lbl.append(cb, ` ${label}`);
    row3.appendChild(lbl);
  }
  toolbar.appendChild(row3);

  // Row 4: All Dependencies as Tree
  const row4 = document.createElement("div");
  row4.className = "dep-toolbar-row";
  row4.appendChild(createRadio("All Dependencies as Tree", "tree"));
  toolbar.appendChild(row4);

  function createRadio(label: string, mode: "conflicts" | "list" | "tree"): HTMLLabelElement {
    const radio = document.createElement("label");
    radio.className = `dep-radio${currentView === mode ? " active" : ""}`;
    const input = document.createElement("input");
    input.type = "radio";
    input.name = "dep-view";
    input.checked = currentView === mode;
    input.addEventListener("change", () => { currentView = mode; renderContent(); renderToolbar(); });
    radio.append(input, ` ${label}`);
    return radio;
  }
}

function renderContent() {
  const container = getContainer();
  if (!currentTree) {
    setPlaceholder(container, "Click Refresh to analyze dependencies");
    return;
  }

  if (currentView === "conflicts") {
    renderConflictsView(container);
  } else if (currentView === "list") {
    renderListView(container);
  } else {
    renderTreeView(container);
  }
}

function matchesSearch(node: { group_id: string; artifact_id: string; version: string }): boolean {
  if (!searchQuery) return true;
  const q = searchQuery.toLowerCase();
  return node.artifact_id.toLowerCase().includes(q) ||
    node.group_id.toLowerCase().includes(q) ||
    node.version.toLowerCase().includes(q);
}

function renderConflictsView(container: HTMLElement) {
  container.replaceChildren();
  const filtered = currentConflicts.filter((c) =>
    matchesSearch({ group_id: c.group_id, artifact_id: c.artifact_id, version: c.versions[0] }),
  );

  if (filtered.length === 0) {
    setPlaceholder(container, "No conflicts found");
    return;
  }

  for (const conflict of filtered) {
    const section = document.createElement("div");
    section.className = "dep-conflict-section";

    const header = document.createElement("div");
    header.className = "dep-conflict-header";
    const prefix = showGroupId ? `${conflict.group_id}:` : "";
    header.textContent = `${prefix}${conflict.artifact_id} — ${conflict.versions.join(" vs ")}`;
    section.appendChild(header);

    for (const node of conflict.nodes) {
      if (hideTestScope && node.scope === "test") continue;
      const row = createDepRow(node, true);
      section.appendChild(row);
    }
    container.appendChild(section);
  }
}

function renderListView(container: HTMLElement) {
  container.replaceChildren();

  let deps = currentFlat;
  if (hideTestScope) deps = deps.filter((d) => d.scope !== "test");
  if (searchQuery) deps = deps.filter(matchesSearch);

  if (deps.length === 0) {
    setPlaceholder(container, "Nothing to show");
    return;
  }

  for (const dep of deps) {
    const row = document.createElement("div");
    row.className = `dep-list-item${dep.is_conflict ? " dep-conflict" : ""}`;
    const prefix = showGroupId ? `${dep.group_id}:` : "";
    let text = `${prefix}${dep.artifact_id} : ${dep.version}`;
    if (dep.omitted_for) text += ` (omitted for conflict with ${dep.omitted_for})`;
    row.textContent = text;
    container.appendChild(row);
  }
}

function renderTreeView(container: HTMLElement) {
  container.replaceChildren();
  if (!currentTree) return;

  const conflictKeys = new Set(currentConflicts.map((c) => `${c.group_id}:${c.artifact_id}`));

  function renderNode(node: DepNode, depth: number): HTMLElement | null {
    if (!node.artifact_id) {
      // root — render children directly
      const frag = document.createElement("div");
      for (const c of node.children) {
        const el = renderNode(c, 0);
        if (el) frag.appendChild(el);
      }
      return frag;
    }

    if (hideTestScope && node.scope === "test") return null;

    const isConflict = conflictKeys.has(`${node.group_id}:${node.artifact_id}`);
    const matches = matchesSearch(node);
    const childEls: HTMLElement[] = [];
    for (const c of node.children) {
      const el = renderNode(c, depth + 1);
      if (el) childEls.push(el);
    }

    if (!matches && childEls.length === 0) return null;

    const wrapper = document.createElement("div");
    wrapper.className = "dep-tree-node";

    const row = document.createElement("div");
    row.className = `dep-tree-row${isConflict ? " dep-conflict" : ""}`;
    row.style.paddingLeft = `${depth * 20 + 8}px`;

    const prefix = showGroupId ? `${node.group_id}:` : "";
    let text = `${prefix}${node.artifact_id}:${node.version}`;
    if (node.scope !== "compile") text += ` [${node.scope}]`;
    if (node.omitted_for) text += ` (omitted for conflict with ${node.omitted_for})`;
    row.textContent = text;

    if (isConflict && !node.omitted_for) {
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        showExcludeMenu(e.clientX, e.clientY, node);
      });
    }

    wrapper.appendChild(row);
    for (const el of childEls) wrapper.appendChild(el);
    return wrapper;
  }

  const tree = renderNode(currentTree, 0);
  if (tree) container.appendChild(tree);
}

function createDepRow(node: MavenConflictNode, showPath: boolean): HTMLElement {
  const row = document.createElement("div");
  row.className = "dep-conflict-row";

  const prefix = showGroupId ? `${node.group_id}:` : "";
  let text = `${prefix}${node.artifact_id}:${node.version}`;
  if (node.scope !== "compile") text += ` [${node.scope}]`;
  if (node.omitted_for) text += ` (omitted → ${node.omitted_for})`;

  if (showPath && node.dep_path.length > 0) {
    text += `  ← ${node.dep_path.join(" → ")}`;
  }

  row.textContent = text;

  if (!node.omitted_for) {
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      showExcludeMenu(e.clientX, e.clientY, node);
    });
  }

  return row;
}

// ---- Exclude context menu ----

function showExcludeMenu(
  x: number, y: number,
  node: { group_id: string; artifact_id: string; direct_parent?: DepCoordRef },
) {
  let menu = document.getElementById("dep-exclude-menu") as HTMLElement | null;
  if (!menu) {
    menu = document.createElement("div");
    menu.id = "dep-exclude-menu";
    menu.className = "context-menu";
    document.body.appendChild(menu);
  }
  menu.replaceChildren();

  const directParent = node.direct_parent;
  if (!directParent || !currentPomPath) {
    menu.classList.add("hidden");
    return;
  }

  const item = document.createElement("div");
  item.className = "context-menu-item";
  item.textContent = `Exclude ${node.artifact_id} from ${directParent.artifact_id}`;
  item.addEventListener("click", async () => {
    menu!.classList.add("hidden");
    await excludeDependency(
      currentPomPath!, directParent.group_id, directParent.artifact_id,
      node.group_id, node.artifact_id,
    );
  });
  menu.appendChild(item);

  const maxX = window.innerWidth - 300;
  const maxY = window.innerHeight - 60;
  menu.style.left = `${Math.min(x, maxX)}px`;
  menu.style.top = `${Math.min(y, maxY)}px`;
  menu.classList.remove("hidden");

  const dismiss = (e: MouseEvent) => {
    if (!menu!.contains(e.target as Node)) {
      menu!.classList.add("hidden");
      document.removeEventListener("mousedown", dismiss, true);
    }
  };
  setTimeout(() => document.addEventListener("mousedown", dismiss, true), 0);
}

// ---- Public API ----

export function isDepAnalyzerActive(): boolean {
  return document.getElementById("dep-analyzer")?.style.display === "flex";
}

export function resetDepAnalyzerState() {
  currentPomPath = null;
  currentTree = null;
  currentConflicts = [];
  currentFlat = [];
  isLoading = false;
  preloadPromise = null;
  hideDepAnalyzer();
}

export async function showDepAnalyzer(pomPath: string) {
  currentPomPath = pomPath;

  const analyzer = document.getElementById("dep-analyzer")!;
  const editorContainer = document.getElementById("editor-container")!;
  analyzer.style.display = "flex";
  editorContainer.style.display = "none";

  updateSubTabs("dep");
  renderToolbar();

  if (currentTree) {
    renderContent();
  } else if (isLoading && preloadPromise) {
    setPlaceholder(getContainer(), "Loading dependency tree...");
    await preloadPromise;
    if (currentTree) renderContent();
    else setPlaceholder(getContainer(), "mvn dependency:tree failed", true);
  } else {
    refreshDependencyTree();
  }
}

export function hideDepAnalyzer() {
  const analyzer = document.getElementById("dep-analyzer")!;
  const editorContainer = document.getElementById("editor-container")!;
  analyzer.style.display = "none";
  editorContainer.style.display = "";

  updateSubTabs("text");
}

export function updateSubTabs(active: "text" | "dep") {
  const bar = document.getElementById("editor-sub-tabs")!;
  bar.querySelectorAll(".editor-sub-tab").forEach((el) => {
    (el as HTMLElement).classList.toggle("active", el.getAttribute("data-sub") === active);
  });
}

export function showSubTabsForFile(filePath: string | null) {
  const bar = document.getElementById("editor-sub-tabs");
  if (!bar) { console.error("editor-sub-tabs element not found"); return; }
  const isPom = !!filePath && (filePath.endsWith("/pom.xml") || filePath.endsWith("\\pom.xml") || filePath === "pom.xml");
  bar.style.display = isPom ? "flex" : "none";

  if (isPom && filePath) {
    preloadDependencyTree(filePath);
  } else if (!isPom && isDepAnalyzerActive()) {
    hideDepAnalyzer();
  }
}

let preloadPromise: Promise<void> | null = null;

function preloadDependencyTree(pomPath: string) {
  if (currentPomPath === pomPath && (currentTree || isLoading)) return;
  currentPomPath = pomPath;
  currentTree = null;
  currentConflicts = [];
  currentFlat = [];
  isLoading = true;
  preloadPromise = doFetchTree(pomPath);
}

async function doFetchTree(pomPath: string) {
  if (!app.currentProjectPath) return;
  isLoading = true;
  const moduleDir = pomPath.replace(/\/pom\.xml$/, "");
  try {
    const result = await mavenDependencyTree(moduleDir);
    // mvn 可能跑数十秒,期间用户可能已切到别的 pom/项目——陈旧结果直接丢弃,
    // 否则 A 的树会渲染在 B 的面板下,右键 exclude 还会写错 pom
    // (同 project-manager doRefreshTree 的守卫模式)
    if (currentPomPath !== pomPath) return;
    if (result.exit_code !== 0 || !result.root) {
      isLoading = false;
      return;
    }
    currentTree = result.root;
    currentConflicts = result.conflicts;
    currentFlat = result.flat;
    isLoading = false;
    if (isDepAnalyzerActive()) renderContent();
  } catch {
    if (currentPomPath === pomPath) isLoading = false;
  }
}

export async function refreshDependencyTree() {
  if (!currentPomPath || !app.currentProjectPath) return;
  isLoading = true;
  const container = getContainer();
  setPlaceholder(container, "Loading...");
  currentTree = null;
  currentConflicts = [];
  currentFlat = [];
  preloadPromise = doFetchTree(currentPomPath);
  await preloadPromise;
  if (!currentTree) {
    setPlaceholder(container, "mvn dependency:tree failed", true);
  }
}
