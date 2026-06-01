import { runMavenCollect, readFile, writeFile } from "./tauri-api";
import { app } from "./state";
import { showStatus } from "./utils";

// ---- Types ----

export interface DepNode {
  groupId: string;
  artifactId: string;
  type: string;
  version: string;
  scope: string;
  omittedFor?: string;
  children: DepNode[];
  depth: number;
  parent?: DepNode;
}

export interface Conflict {
  groupId: string;
  artifactId: string;
  versions: string[];
  nodes: DepNode[];
}

// ---- Parse dependency:tree output ----

function parseDepCoord(raw: string): { groupId: string; artifactId: string; type: string; version: string; scope: string; omittedFor?: string } | null {
  // Strip wrapping parens for omitted entries: "(group:artifact:type:ver:scope - omitted for conflict with X)"
  let text = raw.trim();
  let omittedFor: string | undefined;

  if (text.startsWith("(") && text.endsWith(")")) {
    text = text.slice(1, -1);
    const omitMatch = text.match(/\s*-\s*omitted for conflict with\s+([\w.\-]+)\s*$/);
    if (omitMatch) {
      omittedFor = omitMatch[1];
      text = text.slice(0, text.length - omitMatch[0].length);
    }
  }

  // Also handle "version managed from X" or other suffixes
  const managedMatch = text.match(/\s*\(.*?\)\s*$/);
  if (managedMatch) text = text.slice(0, text.length - managedMatch[0].length);

  const parts = text.split(":");
  if (parts.length < 4) return null;

  // groupId:artifactId:type:version[:scope]  or  groupId:artifactId:type:classifier:version:scope
  if (parts.length === 4) {
    return { groupId: parts[0], artifactId: parts[1], type: parts[2], version: parts[3], scope: "compile", omittedFor };
  }
  if (parts.length === 5) {
    return { groupId: parts[0], artifactId: parts[1], type: parts[2], version: parts[3], scope: parts[4], omittedFor };
  }
  if (parts.length >= 6) {
    return { groupId: parts[0], artifactId: parts[1], type: parts[2], version: parts[4], scope: parts[5], omittedFor };
  }
  return null;
}

export function parseDependencyTree(output: string): DepNode {
  const root: DepNode = {
    groupId: "", artifactId: "", type: "", version: "", scope: "compile",
    children: [], depth: -1,
  };
  const stack: DepNode[] = [root];

  for (const raw of output.split("\n")) {
    const line = raw.replace(/\r$/, "");

    // Match lines like: "[INFO] +- group:artifact:type:ver:scope"
    // Or root: "[INFO] group:artifact:type:ver"
    const m = line.match(/^\[INFO\]\s*((?:[|+\\\- ]+)?)(.+)$/);
    if (!m) continue;

    const [, treePrefix, coordStr] = m;

    // Skip non-dependency lines
    if (coordStr.startsWith("---") || coordStr.startsWith("BUILD") ||
      coordStr.startsWith("Downloading") || coordStr.startsWith("Downloaded") ||
      coordStr.startsWith("Verbose") || coordStr.trim() === "" ||
      !coordStr.includes(":")) continue;

    const coord = parseDepCoord(coordStr);
    if (!coord) continue;

    // Calculate depth from tree prefix chars. Each level is 3 chars: "+- " or "|  " or "\- "
    const prefixClean = treePrefix.replace(/\s+$/, "");
    const depth = prefixClean.length === 0 ? 0 : Math.floor((prefixClean.length + 1) / 3);

    const node: DepNode = {
      groupId: coord.groupId, artifactId: coord.artifactId,
      type: coord.type, version: coord.version, scope: coord.scope,
      children: [], depth,
      ...(coord.omittedFor ? { omittedFor: coord.omittedFor } : {}),
    };

    while (stack.length > depth + 1) stack.pop();
    const parent = stack[stack.length - 1];
    node.parent = parent;
    parent.children.push(node);
    stack.push(node);
  }

  return root;
}

// ---- Conflict detection ----

export function detectConflicts(root: DepNode): Conflict[] {
  const map = new Map<string, DepNode[]>();

  function walk(node: DepNode) {
    if (node.artifactId) {
      const key = `${node.groupId}:${node.artifactId}`;
      let list = map.get(key);
      if (!list) { list = []; map.set(key, list); }
      list.push(node);
    }
    for (const c of node.children) walk(c);
  }
  walk(root);

  const conflicts: Conflict[] = [];
  for (const [key, nodes] of map) {
    const versions = [...new Set(nodes.map((n) => n.version))];
    if (versions.length > 1 || nodes.some((n) => n.omittedFor)) {
      const allVersions = [...new Set([...versions, ...nodes.filter((n) => n.omittedFor).map((n) => n.omittedFor!)])];
      const [groupId, artifactId] = key.split(":");
      conflicts.push({ groupId, artifactId, versions: allVersions, nodes });
    }
  }

  return conflicts.sort((a, b) => a.artifactId.localeCompare(b.artifactId));
}

// ---- Flatten for list view ----

interface FlatDep {
  groupId: string;
  artifactId: string;
  version: string;
  scope: string;
  isConflict: boolean;
  omittedFor?: string;
}

function flattenUnique(root: DepNode, conflictKeys: Set<string>): FlatDep[] {
  const seen = new Map<string, FlatDep>();
  function walk(node: DepNode) {
    if (node.artifactId) {
      const key = `${node.groupId}:${node.artifactId}:${node.version}`;
      if (!seen.has(key)) {
        const ck = `${node.groupId}:${node.artifactId}`;
        seen.set(key, {
          groupId: node.groupId, artifactId: node.artifactId,
          version: node.version, scope: node.scope,
          isConflict: conflictKeys.has(ck),
          omittedFor: node.omittedFor,
        });
      }
    }
    for (const c of node.children) walk(c);
  }
  walk(root);
  return [...seen.values()].sort((a, b) => a.artifactId.localeCompare(b.artifactId));
}

// ---- State ----

let currentPomPath: string | null = null;
let currentTree: DepNode | null = null;
let currentConflicts: Conflict[] = [];
let currentView: "conflicts" | "list" | "tree" = "list";
let searchQuery = "";
let showGroupId = false;
let hideTestScope = false;
let isLoading = false;
let showSize = false;

// ---- Exclude dependency (surgical pom.xml edit) ----

export async function excludeDependency(
  pomPath: string,
  parentGroupId: string, parentArtifactId: string,
  excludeGroupId: string, excludeArtifactId: string,
): Promise<void> {
  const content = await readFile(pomPath);
  const lines = content.split("\n");

  // Find the <dependency> block matching parent
  let depStart = -1;
  let depEnd = -1;
  let foundGroup = false;
  let foundArtifact = false;

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (trimmed === "<dependency>") {
      depStart = i;
      foundGroup = false;
      foundArtifact = false;
    }
    if (depStart >= 0) {
      if (trimmed === `<groupId>${parentGroupId}</groupId>`) foundGroup = true;
      if (trimmed === `<artifactId>${parentArtifactId}</artifactId>`) foundArtifact = true;
      if (trimmed === "</dependency>") {
        if (foundGroup && foundArtifact) {
          depEnd = i;
          break;
        }
        depStart = -1;
      }
    }
  }

  if (depStart < 0 || depEnd < 0) {
    showStatus(`Cannot find <dependency> for ${parentGroupId}:${parentArtifactId}`, true);
    return;
  }

  // Detect indentation
  const depIndent = lines[depStart].match(/^(\s*)/)?.[1] ?? "        ";
  const childIndent = depIndent + "    ";
  const grandChildIndent = childIndent + "    ";

  // Check if <exclusions> already exists within this dependency block
  let exclusionsStart = -1;
  let exclusionsEnd = -1;
  for (let i = depStart; i <= depEnd; i++) {
    if (lines[i].trim() === "<exclusions>") exclusionsStart = i;
    if (lines[i].trim() === "</exclusions>") exclusionsEnd = i;
  }

  const exclusionBlock = [
    `${grandChildIndent}<exclusion>`,
    `${grandChildIndent}    <groupId>${excludeGroupId}</groupId>`,
    `${grandChildIndent}    <artifactId>${excludeArtifactId}</artifactId>`,
    `${grandChildIndent}</exclusion>`,
  ];

  if (exclusionsStart >= 0 && exclusionsEnd >= 0) {
    // Insert before </exclusions>
    lines.splice(exclusionsEnd, 0, ...exclusionBlock);
  } else {
    // Insert <exclusions>...</exclusions> before </dependency>
    const newLines = [
      `${childIndent}<exclusions>`,
      ...exclusionBlock,
      `${childIndent}</exclusions>`,
    ];
    lines.splice(depEnd, 0, ...newLines);
  }

  await writeFile(pomPath, lines.join("\n"));
  showStatus(`Excluded ${excludeGroupId}:${excludeArtifactId}`);
}

// ---- Find which direct dependency brings in a transitive ----

function findDirectParent(node: DepNode): DepNode | null {
  let cur = node;
  while (cur.parent && cur.parent.depth >= 0) {
    if (cur.parent.depth === 0) return cur.parent;
    cur = cur.parent;
  }
  return cur.depth === 0 ? null : cur;
}

// ---- UI Rendering ----

function getContainer(): HTMLElement {
  return document.getElementById("dep-analyzer-content")!;
}

function renderToolbar() {
  const toolbar = document.getElementById("dep-analyzer-toolbar")!;
  toolbar.innerHTML = "";

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
    container.innerHTML = '<div class="dep-placeholder">Click Refresh to analyze dependencies</div>';
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

function matchesSearch(node: { groupId: string; artifactId: string; version: string }): boolean {
  if (!searchQuery) return true;
  const q = searchQuery.toLowerCase();
  return node.artifactId.toLowerCase().includes(q) ||
    node.groupId.toLowerCase().includes(q) ||
    node.version.toLowerCase().includes(q);
}

function renderConflictsView(container: HTMLElement) {
  container.innerHTML = "";
  const filtered = currentConflicts.filter((c) =>
    matchesSearch({ groupId: c.groupId, artifactId: c.artifactId, version: c.versions[0] }),
  );

  if (filtered.length === 0) {
    container.innerHTML = '<div class="dep-placeholder">No conflicts found</div>';
    return;
  }

  for (const conflict of filtered) {
    const section = document.createElement("div");
    section.className = "dep-conflict-section";

    const header = document.createElement("div");
    header.className = "dep-conflict-header";
    const prefix = showGroupId ? `${conflict.groupId}:` : "";
    header.textContent = `${prefix}${conflict.artifactId} — ${conflict.versions.join(" vs ")}`;
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
  container.innerHTML = "";
  if (!currentTree) return;

  const conflictKeys = new Set(currentConflicts.map((c) => `${c.groupId}:${c.artifactId}`));
  let deps = flattenUnique(currentTree, conflictKeys);

  if (hideTestScope) deps = deps.filter((d) => d.scope !== "test");
  if (searchQuery) deps = deps.filter(matchesSearch);

  if (deps.length === 0) {
    container.innerHTML = '<div class="dep-placeholder">Nothing to show</div>';
    return;
  }

  for (const dep of deps) {
    const row = document.createElement("div");
    row.className = `dep-list-item${dep.isConflict ? " dep-conflict" : ""}`;
    const prefix = showGroupId ? `${dep.groupId}:` : "";
    let text = `${prefix}${dep.artifactId} : ${dep.version}`;
    if (dep.omittedFor) text += ` (omitted for conflict with ${dep.omittedFor})`;
    row.textContent = text;
    container.appendChild(row);
  }
}

function renderTreeView(container: HTMLElement) {
  container.innerHTML = "";
  if (!currentTree) return;

  const conflictKeys = new Set(currentConflicts.map((c) => `${c.groupId}:${c.artifactId}`));

  function renderNode(node: DepNode, depth: number): HTMLElement | null {
    if (!node.artifactId) {
      // root — render children directly
      const frag = document.createElement("div");
      for (const c of node.children) {
        const el = renderNode(c, 0);
        if (el) frag.appendChild(el);
      }
      return frag;
    }

    if (hideTestScope && node.scope === "test") return null;

    const isConflict = conflictKeys.has(`${node.groupId}:${node.artifactId}`);
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

    const prefix = showGroupId ? `${node.groupId}:` : "";
    let text = `${prefix}${node.artifactId}:${node.version}`;
    if (node.scope !== "compile") text += ` [${node.scope}]`;
    if (node.omittedFor) text += ` (omitted for conflict with ${node.omittedFor})`;
    row.textContent = text;

    if (isConflict && !node.omittedFor) {
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

function createDepRow(node: DepNode, showPath: boolean): HTMLElement {
  const row = document.createElement("div");
  row.className = "dep-conflict-row";

  const prefix = showGroupId ? `${node.groupId}:` : "";
  let text = `${prefix}${node.artifactId}:${node.version}`;
  if (node.scope !== "compile") text += ` [${node.scope}]`;
  if (node.omittedFor) text += ` (omitted → ${node.omittedFor})`;

  if (showPath) {
    const path = getDepPath(node);
    if (path) text += `  ← ${path}`;
  }

  row.textContent = text;

  if (!node.omittedFor) {
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      showExcludeMenu(e.clientX, e.clientY, node);
    });
  }

  return row;
}

function getDepPath(node: DepNode): string {
  const parts: string[] = [];
  let cur = node.parent;
  while (cur && cur.artifactId) {
    parts.unshift(cur.artifactId);
    cur = cur.parent;
  }
  return parts.join(" → ");
}

// ---- Exclude context menu ----

function showExcludeMenu(x: number, y: number, node: DepNode) {
  let menu = document.getElementById("dep-exclude-menu") as HTMLElement | null;
  if (!menu) {
    menu = document.createElement("div");
    menu.id = "dep-exclude-menu";
    menu.className = "context-menu";
    document.body.appendChild(menu);
  }
  menu.innerHTML = "";

  const directParent = findDirectParent(node);
  if (!directParent || !currentPomPath) {
    menu.classList.add("hidden");
    return;
  }

  const item = document.createElement("div");
  item.className = "context-menu-item";
  item.textContent = `Exclude ${node.artifactId} from ${directParent.artifactId}`;
  item.addEventListener("click", async () => {
    menu!.classList.add("hidden");
    await excludeDependency(
      currentPomPath!, directParent.groupId, directParent.artifactId,
      node.groupId, node.artifactId,
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
    getContainer().innerHTML = '<div class="dep-placeholder">Loading dependency tree...</div>';
    await preloadPromise;
    if (currentTree) renderContent();
    else getContainer().innerHTML = '<div class="dep-placeholder dep-error">mvn dependency:tree failed</div>';
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
  isLoading = true;
  preloadPromise = doFetchTree(pomPath);
}

async function doFetchTree(pomPath: string) {
  if (!app.currentProjectPath) return;
  isLoading = true;
  const moduleDir = pomPath.replace(/\/pom\.xml$/, "");
  try {
    const result = await runMavenCollect(moduleDir, [
      "dependency:tree",
    ]);
    if (result.exit_code !== 0) {
      isLoading = false;
      return;
    }
    currentTree = parseDependencyTree(result.output);
    currentConflicts = detectConflicts(currentTree);
    isLoading = false;
    if (isDepAnalyzerActive()) renderContent();
  } catch {
    isLoading = false;
  }
}

export async function refreshDependencyTree() {
  if (!currentPomPath || !app.currentProjectPath) return;
  isLoading = true;
  const container = getContainer();
  container.innerHTML = '<div class="dep-placeholder">Loading...</div>';
  currentTree = null;
  currentConflicts = [];
  preloadPromise = doFetchTree(currentPomPath);
  await preloadPromise;
  if (!currentTree) {
    container.innerHTML = '<div class="dep-placeholder dep-error">mvn dependency:tree failed</div>';
  }
}
