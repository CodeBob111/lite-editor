import type { EditorView } from "@codemirror/view";
import { app } from "./state";
import { escapeHtml, showStatus } from "./utils";
import { openFileAtLine } from "./file-ops";
import {
  lspIsReady, lspGotoDefinition, lspFindReferences,
  findClassInMaven, searchJavaClass, lspDidChange,
  searchInFiles, listAllFiles, readFile, queryUsages,
} from "./tauri-api";

let usagesPopupIndex = 0;
let usagesPopupItems: { file: string; line: number; text: string }[] = [];
let lspNavigationInFlight = false;

function isLspBusyError(error: unknown): boolean {
  const text = String(error).toLowerCase();
  return text.includes("timed out") || text.includes("timeout") || text.includes("cancelled");
}

// 给 LSP 请求加 JS 侧超时:jdtls 繁忙/索引时,单次请求在 Rust 侧最长会等若干秒并独占
// 响应通道(request_and_wait 持锁直到超时),整个导航像冻住。超时即放弃、走快速文本搜索
// 兜底,保证「跳转定义 / 查引用」不卡死。
function withTimeout<T>(p: Promise<T>, ms: number, label = "lsp"): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) => setTimeout(() => reject(new Error(`${label} timeout`)), ms)),
  ]);
}

function showLspBusyStatus() {
  showStatus("LSP is busy indexing. Try again in a few seconds.", true);
}

let onRevealDirectory: ((path: string) => void) | null = null;
export function setRevealDirectoryHandler(fn: (path: string) => void) {
  onRevealDirectory = fn;
}

interface ImportContext {
  fqn: string;
  segment: string;
  isClass: boolean;
  packagePrefix: string;
}

function getImportContext(view: EditorView, pos: number): ImportContext | null {
  const line = view.state.doc.lineAt(pos);
  const text = line.text;
  const match = text.match(/^import\s+(?:static\s+)?([\w.]+)\s*;/);
  if (!match) return null;

  const fqn = match[1];
  const fqnStart = text.indexOf(match[1]);
  const relCol = pos - line.from - fqnStart;
  if (relCol < 0 || relCol > fqn.length) return null;

  const parts = fqn.split(".");
  let offset = 0;
  for (let i = 0; i < parts.length; i++) {
    if (relCol >= offset && relCol <= offset + parts[i].length) {
      const segment = parts[i];
      const isClass = /^[A-Z]/.test(segment);
      const packagePrefix = isClass
        ? parts.slice(0, i).join(".")
        : parts.slice(0, i + 1).join(".");
      return { fqn, segment, isClass, packagePrefix };
    }
    offset += parts[i].length + 1;
  }
  return null;
}

async function navigateToPackageDir(packageFqn: string) {
  if (!app.currentProjectPath || !onRevealDirectory) {
    showStatus(`Package: ${packageFqn}`);
    return;
  }
  const packagePath = packageFqn.replace(/\./g, "/");
  const marker = `/${packagePath}/`;
  try {
    const allFiles = await listAllFiles(app.currentProjectPath);
    const matchingFile = allFiles.find((f) => f.includes(marker));
    if (matchingFile) {
      const idx = matchingFile.indexOf(marker);
      const dirPath = matchingFile.substring(0, idx + marker.length - 1);
      onRevealDirectory(dirPath);
      showStatus(`Package: ${packageFqn}`);
      return;
    }
  } catch { /* fall through */ }
  showStatus(`Package ${packageFqn} not found in project source`);
}

function resolveImportForClass(view: EditorView, className: string): string | null {
  const doc = view.state.doc;
  for (let i = 1; i <= Math.min(doc.lines, 100); i++) {
    const text = doc.line(i).text;
    if (!text.startsWith("import ")) {
      if (text.startsWith("package ") || text.trim() === "") continue;
      if (text.startsWith("public ") || text.startsWith("class ") || text.startsWith("@") || text.startsWith("/*")) break;
      continue;
    }
    const m = text.match(/^import\s+([\w.]+\.(\w+))\s*;/);
    if (m && m[2] === className) return m[1];
  }
  return null;
}

function resolveReceiverAndMethod(view: EditorView, pos: number): { typeName: string; methodName: string } | null {
  const line = view.state.doc.lineAt(pos);
  const text = line.text;
  const col = pos - line.from;
  let start = col;
  while (start > 0 && /\w/.test(text[start - 1])) start--;
  let end = col;
  while (end < text.length && /\w/.test(text[end])) end++;
  const methodName = text.slice(start, end);

  if (start < 2 || text[start - 1] !== ".") return null;
  let recEnd = start - 1;
  let recStart = recEnd;
  while (recStart > 0 && /\w/.test(text[recStart - 1])) recStart--;
  const receiver = text.slice(recStart, recEnd);
  if (!receiver || !methodName) return null;

  // 接收者大写开头 = 静态调用(如 MtopResultUtil.foo()),它本身就是类型名,直接用。
  // 否则是实例调用(someVar.foo()),下面去找 `类型 someVar` 声明推断变量类型。
  if (/^[A-Z]/.test(receiver)) {
    return { typeName: receiver, methodName };
  }

  const doc = view.state.doc;
  const escaped = receiver.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(`\\b([A-Z]\\w*)(?:<[^>]*>)?\\s+${escaped}\\b`);
  for (let i = 1; i <= doc.lines; i++) {
    const m = doc.line(i).text.match(pattern);
    if (m) return { typeName: m[1], methodName };
  }
  return null;
}

async function findMethodLineInFile(filePath: string, methodName: string): Promise<number> {
  try {
    const content = await readFile(filePath);
    const lines = content.split("\n");
    const pattern = new RegExp(`\\b${methodName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\(`);
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (pattern.test(line) && /^\s*(public|protected|private|static|abstract|default|synchronized|\w+\s+\w)/.test(line.trimStart())) {
        return i + 1;
      }
    }
    for (let i = 0; i < lines.length; i++) {
      if (pattern.test(lines[i])) return i + 1;
    }
  } catch { /* fall through */ }
  return 1;
}

// 不依赖任何索引:按 import 推出的 FQN,在工程源码里按相对路径直接找到 .java 文件。
// jdtls / java 索引都没建好时也能跳到源码(刚打开新工程的核心场景)。
async function findProjectClassFileByFqn(fqn: string): Promise<string | null> {
  if (!app.currentProjectPath) return null;
  try {
    const rel = "/" + fqn.replace(/\./g, "/") + ".java";
    const all = await listAllFiles(app.currentProjectPath);
    return all.find((p) => p.endsWith(rel)) ?? null;
  } catch {
    return null;
  }
}

async function tryNavigateToReceiverType(view: EditorView, pos: number, filePath: string): Promise<boolean> {
  const resolved = resolveReceiverAndMethod(view, pos);
  if (!resolved) return false;
  const { typeName, methodName } = resolved;

  async function jumpToFile(targetPath: string) {
    const line = await findMethodLineInFile(targetPath, methodName);
    openFileAtLine(targetPath, line);
    showStatus(`Jumped to ${targetPath.split("/").pop()}:${line}`);
  }

  if (app.javaIndexReady && app.currentProjectPath) {
    const locations = await searchJavaClass(app.currentProjectPath, typeName);
    const otherFiles = locations.filter((l) => l.path !== filePath);
    if (otherFiles.length === 1) {
      await jumpToFile(otherFiles[0].path);
      return true;
    }
    if (otherFiles.length > 1) {
      const fqn = resolveImportForClass(view, typeName);
      const match = fqn ? otherFiles.find((l) => l.fqn === fqn) : null;
      if (match) {
        await jumpToFile(match.path);
        return true;
      }
    }
  }

  const fqn = resolveImportForClass(view, typeName);
  if (fqn) {
    // 先在工程源码里按 FQN 路径直接找(不依赖索引);找不到再去 Maven 反编译。
    const projFile = await findProjectClassFileByFqn(fqn);
    if (projFile) {
      const line = await findMethodLineInFile(projFile, methodName);
      openFileAtLine(projFile, line);
      showStatus(`Jumped to ${projFile.split("/").pop()}:${line}`);
      return true;
    }
    showStatus(`Looking up ${fqn}...`);
    try {
      const result = await findClassInMaven(fqn);
      if (result) {
        const line = await findMethodLineInFile(result.path, methodName);
        openFileAtLine(result.path, line);
        showStatus(`Opened source: ${fqn}`);
        return true;
      }
    } catch { /* fall through */ }
  }
  return false;
}

function findMethodDefinitionInFile(view: EditorView, methodName: string, callLine: number): number {
  const doc = view.state.doc;
  const escaped = methodName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const sigPattern = new RegExp(`\\b${escaped}\\s*\\(`);
  const modifiers = /^\s*(public|protected|private|static|abstract|synchronized|final|default|native)\b/;
  const typeReturn = new RegExp(`\\b\\w+(?:<[^>]*>)?(?:\\[\\])?\\s+${escaped}\\s*\\(`);
  for (let i = 1; i <= doc.lines; i++) {
    if (i === callLine) continue;
    const text = doc.line(i).text;
    if (!sigPattern.test(text)) continue;
    if (modifiers.test(text) && typeReturn.test(text)) return i;
  }
  for (let i = 1; i <= doc.lines; i++) {
    if (i === callLine) continue;
    const text = doc.line(i).text;
    if (typeReturn.test(text)) return i;
  }
  return 0;
}

function resolveImportForSymbol(view: EditorView, pos: number): string | null {
  const wordRange = view.state.wordAt(pos);
  if (!wordRange) return null;
  const symbol = view.state.sliceDoc(wordRange.from, wordRange.to);
  if (!symbol || symbol.length < 2) return null;

  const doc = view.state.doc;
  for (let i = 1; i <= Math.min(doc.lines, 100); i++) {
    const text = doc.line(i).text;
    if (!text.startsWith("import ")) {
      if (text.startsWith("package ") || text.trim() === "") continue;
      if (text.startsWith("public ") || text.startsWith("class ") || text.startsWith("@") || text.startsWith("/*")) break;
      continue;
    }
    const m = text.match(/^import\s+([\w.]+\.(\w+))\s*;/);
    if (m && m[2] === symbol) return m[1];
  }
  return null;
}

function getWordAtPos(view: EditorView, pos: number): string {
  const line = view.state.doc.lineAt(pos);
  const text = line.text;
  const col = pos - line.from;
  let start = col, end = col;
  while (start > 0 && /\w/.test(text[start - 1])) start--;
  while (end < text.length && /\w/.test(text[end])) end++;
  return text.slice(start, end) || "symbol";
}

function getCurrentFilePackage(view: EditorView): string | null {
  const doc = view.state.doc;
  for (let i = 1; i <= Math.min(doc.lines, 30); i++) {
    const line = doc.line(i).text.trim();
    if (line.startsWith("package ") && line.endsWith(";")) {
      return line.slice(8, -1).trim();
    }
    if (line.startsWith("import ") || line.startsWith("public ") || line.startsWith("class ") || line.startsWith("@")) break;
  }
  return null;
}

export function showUsagesPopup(view: EditorView, pos: number, symbol: string, items: { file: string; line: number; text: string }[]) {
  const popup = document.getElementById("usages-popup")!;
  const titleEl = document.getElementById("usages-popup-title")!;
  const countEl = document.getElementById("usages-popup-count")!;
  const listEl = document.getElementById("usages-popup-list")!;

  if (items.length === 0) {
    popup.classList.add("hidden");
    showStatus("No usages found");
    return;
  }

  titleEl.textContent = symbol;
  countEl.textContent = `${items.length} usage(s)`;
  usagesPopupIndex = 0;
  usagesPopupItems = items;

  listEl.innerHTML = "";
  for (let i = 0; i < items.length; i++) {
    const u = items[i];
    const el = document.createElement("div");
    el.className = `usages-popup-item${i === 0 ? " selected" : ""}`;
    const filename = u.file.split("/").pop() || u.file;
    el.innerHTML = `<span class="usages-popup-file">${escapeHtml(filename)}</span><span class="usages-popup-line">${u.line}</span><span class="usages-popup-text">${escapeHtml(u.text)}</span>`;
    el.addEventListener("click", () => {
      hideUsagesPopup();
      openFileAtLine(u.file, u.line);
    });
    el.addEventListener("mouseenter", () => {
      usagesPopupIndex = i;
      listEl.querySelectorAll(".usages-popup-item").forEach((el, j) => el.classList.toggle("selected", j === i));
      updateUsagesPopupPath();
    });
    listEl.appendChild(el);
  }

  updateUsagesPopupPath();

  const coords = view.coordsAtPos(pos);
  if (coords) {
    const left = Math.min(coords.left, window.innerWidth - 720);
    const top = coords.bottom + 4;
    const clampedTop = Math.min(top, window.innerHeight - 340);
    popup.style.left = `${Math.max(0, left)}px`;
    popup.style.top = `${Math.max(0, clampedTop)}px`;
  }

  popup.classList.remove("hidden");
}

function updateUsagesPopupPath() {
  const pathEl = document.getElementById("usages-popup-path")!;
  if (usagesPopupIndex >= 0 && usagesPopupIndex < usagesPopupItems.length) {
    const relPath = app.currentProjectPath
      ? usagesPopupItems[usagesPopupIndex].file.replace(app.currentProjectPath + "/", "")
      : usagesPopupItems[usagesPopupIndex].file;
    pathEl.textContent = relPath;
  }
}

export function hideUsagesPopup() {
  document.getElementById("usages-popup")!.classList.add("hidden");
  usagesPopupItems = [];
}

export function usagesPopupNavigate(delta: number) {
  const listEl = document.getElementById("usages-popup-list")!;
  const items = listEl.querySelectorAll(".usages-popup-item");
  if (items.length === 0) return;
  usagesPopupIndex = Math.max(0, Math.min(items.length - 1, usagesPopupIndex + delta));
  items.forEach((el, i) => el.classList.toggle("selected", i === usagesPopupIndex));
  items[usagesPopupIndex]?.scrollIntoView({ block: "nearest" });
  updateUsagesPopupPath();
}

export function usagesPopupConfirm() {
  if (usagesPopupIndex >= 0 && usagesPopupIndex < usagesPopupItems.length) {
    const u = usagesPopupItems[usagesPopupIndex];
    hideUsagesPopup();
    openFileAtLine(u.file, u.line);
  }
}

export async function smartNavigateAtPos(view: EditorView, pos: number) {
  if (!app.currentFilePath) return;
  const filePath = app.currentFilePath;
  if (lspNavigationInFlight) {
    showStatus("LSP request already running...");
    return;
  }
  lspNavigationInFlight = true;

  try {
    const isJava = filePath.endsWith(".java");
    let lspReady = false;
    try {
      lspReady = await lspIsReady(filePath);
    } catch { /* assume not ready */ }

    const line = view.state.doc.lineAt(pos);
    const lineNumber = line.number - 1;
    const character = pos - line.from;

    showStatus("Resolving...");

    let def: { uri: string; line: number } | null = null;
    let lspFailed = false;

    if (lspReady) {
      try {
        def = await withTimeout(lspGotoDefinition(filePath, lineNumber, character), 3500, "definition");
      } catch (e) {
        // 超时不再直接 return,落到下面的类索引 / 文本兜底,避免「卡一下又啥都没有」。
        if (isLspBusyError(e) && !String(e).includes("definition timeout")) { showLspBusyStatus(); return; }
        lspFailed = true;
      }
    }

    if (def) {
      const defFile = def.uri.replace("file://", "");
      const clickedOnDefinition = defFile === filePath && def.line === lineNumber;
      if (!clickedOnDefinition) {
        if (defFile.startsWith("/")) {
          openFileAtLine(defFile, def.line + 1);
          showStatus(`Jumped to ${defFile.split("/").pop()}:${def.line + 1}`);
          return;
        }
        def = null;
      }
    }

    if (!def) {
      if (isJava) {
        const importCtx = getImportContext(view, pos);
        if (importCtx) {
          if (importCtx.isClass) {
            if (app.javaIndexReady && app.currentProjectPath) {
              const locations = await searchJavaClass(app.currentProjectPath, importCtx.segment);
              const match = locations.find((l) => l.fqn === importCtx.fqn);
              if (match) {
                openFileAtLine(match.path, 1);
                showStatus(`Jumped to ${match.path.split("/").pop()}`);
                return;
              }
            }
            showStatus(`Looking up ${importCtx.fqn} in Maven...`);
            try {
              const result = await findClassInMaven(importCtx.fqn);
              if (result) {
                openFileAtLine(result.path, 1);
                showStatus(`Opened decompiled source: ${importCtx.fqn}`);
                return;
              }
            } catch { /* fall through */ }
            showStatus(`Cannot find source for ${importCtx.fqn}`);
            return;
          } else {
            await navigateToPackageDir(importCtx.packagePrefix);
            return;
          }
        }
      }

      if (isJava && app.javaIndexReady && app.currentProjectPath) {
        const word = getWordAtPos(view, pos);
        if (word && word[0] === word[0].toUpperCase() && word.length > 1) {
          const locations = await searchJavaClass(app.currentProjectPath, word);
          const otherFiles = locations.filter((l) => l.path !== filePath);
          if (otherFiles.length === 1) {
            openFileAtLine(otherFiles[0].path, 1);
            showStatus(`Jumped to ${otherFiles[0].path.split("/").pop()}`);
            return;
          } else if (otherFiles.length > 1) {
            const fqn = resolveImportForSymbol(view, pos);
            const match = fqn
              ? otherFiles.find((l) => l.fqn === fqn)
              : null;
            if (match) {
              openFileAtLine(match.path, 1);
              showStatus(`Jumped to ${match.path.split("/").pop()}`);
              return;
            }
            const currentPkg = getCurrentFilePackage(view);
            if (currentPkg) {
              const samePackage = otherFiles.filter((l) => l.package === currentPkg);
              if (samePackage.length === 1) {
                openFileAtLine(samePackage[0].path, 1);
                showStatus(`Jumped to ${samePackage[0].path.split("/").pop()}`);
                return;
              }
            }
            usagesPopupItems = otherFiles.map((l) => ({
              file: l.path,
              line: 1,
              text: l.fqn,
            }));
            showUsagesPopup(view, pos, word, usagesPopupItems);
            showStatus(`${otherFiles.length} matches for ${word}`);
            return;
          }
        }
      }

      if (isJava) {
        if (await tryNavigateToReceiverType(view, pos, filePath)) return;
      }

      if (isJava) {
        const word = getWordAtPos(view, pos);
        if (word && /^[a-z]/.test(word)) {
          const defLine = findMethodDefinitionInFile(view, word, view.state.doc.lineAt(pos).number);
          if (defLine > 0) {
            openFileAtLine(filePath, defLine);
            showStatus(`Jumped to ${word} definition`);
            return;
          }
        }
      }

      const fqn = resolveImportForSymbol(view, pos);
      if (fqn) {
        // 先在工程源码里按 FQN 路径直接找(不依赖索引);找不到再去 Maven 反编译。
        const projFile = await findProjectClassFileByFqn(fqn);
        if (projFile) {
          openFileAtLine(projFile, 1);
          showStatus(`Jumped to ${projFile.split("/").pop()}`);
          return;
        }
        showStatus(`Looking up ${fqn} in Maven repository...`);
        try {
          const result = await findClassInMaven(fqn);
          if (result) {
            openFileAtLine(result.path, 1);
            showStatus(`Opened decompiled source: ${fqn}`);
            return;
          }
        } catch { /* fall through */ }
      }
      // Cmd+点击是「跳转到定义」,不是「查引用」。解析不出定义时如实提示,
      // 不再退回 find-usages(此前会塞一个文本搜索的引用列表,体验上像功能错了)。
      if (isJava && !lspReady && !app.javaIndexReady) {
        showStatus("Java 索引/语言服务尚未就绪,稍后再点");
      } else {
        showStatus("未找到定义");
      }
      return;
    }

    if (isJava) {
      const word = getWordAtPos(view, pos);
      if (word && word[0] === word[0].toUpperCase() && word.length > 1) {
        const fqn = resolveImportForSymbol(view, pos);
        if (fqn) {
          if (app.javaIndexReady && app.currentProjectPath) {
            const locations = await searchJavaClass(app.currentProjectPath, word);
            const match = locations.find((l) => l.fqn === fqn && l.path !== filePath);
            if (match) {
              openFileAtLine(match.path, 1);
              showStatus(`Jumped to ${match.path.split("/").pop()}`);
              return;
            }
          }
          showStatus(`Looking up ${fqn}...`);
          try {
            const result = await findClassInMaven(fqn);
            if (result) {
              openFileAtLine(result.path, 1);
              showStatus(`Opened source: ${fqn}`);
              return;
            }
          } catch { /* fall through to find usages */ }
        }
      }
      if (await tryNavigateToReceiverType(view, pos, filePath)) return;
    }

    showStatus("Finding usages...");
    const usageWord = getWordAtPos(view, pos);

    // 1) 本地「符号出现」倒排索引:瞬时、不依赖 jdtls(项目打开时已后台构建+落盘缓存)。
    //    索引就绪且有命中就直接用,彻底避开 jdtls 索引中的卡顿。
    if (usageWord && app.currentProjectPath) {
      try {
        const hits = await queryUsages(app.currentProjectPath, usageWord, 500);
        if (hits.length > 0) {
          usagesPopupItems = hits
            .filter((h) => !(h.file === filePath && h.line === lineNumber + 1))
            .map((h) => ({ file: h.file, line: h.line, text: h.text }));
          showUsagesPopup(view, pos, usageWord, usagesPopupItems);
          showStatus(`${usagesPopupItems.length} usage(s)`);
          return;
        }
      } catch { /* 索引尚未构建/失败 → 落到 jdtls/文本 */ }
    }

    // 2) jdtls 精确引用,加 4s 超时:jdtls 还在索引时别死等(原来 Rust 侧要等到 8s
    //    且独占响应通道,整个导航卡住)。超时/无 server/空结果一律落到快速文本搜索兜底。
    let lspUsages: Awaited<ReturnType<typeof lspFindReferences>> | null = null;
    if (lspReady) {
      try {
        lspUsages = await withTimeout(lspFindReferences(filePath, lineNumber, character), 4000, "references");
      } catch { /* 超时/繁忙/无 server → 文本兜底 */ }
    }

    if (lspUsages) {
      usagesPopupItems = lspUsages
        .filter((u) => !(u.uri.replace("file://", "") === filePath && u.line === lineNumber))
        .map((u) => ({ file: u.uri.replace("file://", ""), line: u.line + 1, text: u.text }));
      if (usagesPopupItems.length > 0) {
        showUsagesPopup(view, pos, usageWord, usagesPopupItems);
        showStatus(`${usagesPopupItems.length} usage(s) found`);
        return;
      }
    }

    // 兜底:全项目文本搜索(快,不卡)。
    if (usageWord && app.currentProjectPath) {
      showStatus("Finding usages (text search)...");
      try {
        const results = await searchInFiles(app.currentProjectPath, usageWord, true, 200);
        usagesPopupItems = results.map((r) => ({ file: r.path, line: r.line, text: r.text }));
        showUsagesPopup(view, pos, usageWord, usagesPopupItems);
        showStatus(`${results.length} occurrence(s) of "${usageWord}"`);
      } catch {
        showStatus("Find usages failed");
      }
    } else {
      showStatus("No usages found");
    }
  } finally {
    lspNavigationInFlight = false;
  }
}

export async function gotoDefinitionAtCursor(view: EditorView) {
  if (!app.currentFilePath) return;
  const filePath = app.currentFilePath;
  if (lspNavigationInFlight) {
    showStatus("LSP request already running...");
    return;
  }
  lspNavigationInFlight = true;

  try {
    const isJava = filePath.endsWith(".java");
    let lspReady = false;
    try { lspReady = await lspIsReady(filePath); } catch { /* */ }

    const pos = view.state.selection.main.head;
    const line = view.state.doc.lineAt(pos);
    const lineNumber = line.number - 1;
    const character = pos - line.from;

    showStatus("Going to definition...");

    let def: { uri: string; line: number } | null = null;
    if (lspReady) {
      try {
        def = await lspGotoDefinition(filePath, lineNumber, character);
      } catch (e) {
        if (isLspBusyError(e)) { showLspBusyStatus(); return; }
      }
    }

    if (def) {
      const file = def.uri.replace("file://", "");
      openFileAtLine(file, def.line + 1);
      showStatus(`Jumped to ${file.split("/").pop()}:${def.line + 1}`);
    } else if (isJava && app.currentProjectPath) {
      const word = getWordAtPos(view, pos);
      // 类查找需 java 索引就绪;没就绪就跳过,走下面的文本兜底
      if (word && app.javaIndexReady && word[0] === word[0].toUpperCase() && word.length > 1) {
        const locations = await searchJavaClass(app.currentProjectPath, word);
        if (locations.length === 1) {
          openFileAtLine(locations[0].path, 1);
          showStatus(`Jumped to ${locations[0].path.split("/").pop()}`);
          return;
        } else if (locations.length > 1) {
          const fqn = resolveImportForSymbol(view, pos);
          const match = fqn ? locations.find((l) => l.fqn === fqn) : null;
          if (match) {
            openFileAtLine(match.path, 1);
            showStatus(`Jumped to ${match.path.split("/").pop()}`);
            return;
          }
          const currentPkg = getCurrentFilePackage(view);
          if (currentPkg) {
            const samePackage = locations.filter((l) => l.package === currentPkg);
            if (samePackage.length === 1) {
              openFileAtLine(samePackage[0].path, 1);
              showStatus(`Jumped to ${samePackage[0].path.split("/").pop()}`);
              return;
            }
          }
          showStatus(`${locations.length} matches — use Cmd+Click for picker`);
          return;
        }
      }
      // 文本兜底:jdtls/索引没就绪或类查找没命中 → 工程内搜该符号,跳到第一个匹配(与 Cmd+点击一致),
      // 让刚打开、还没建好索引的工程上 F12 也不至于「啥都没有」。
      if (word) {
        const results = await searchInFiles(app.currentProjectPath, word, true, 50);
        if (results.length > 0) {
          openFileAtLine(results[0].path, results[0].line + 1);
          showStatus(`No exact definition — first match of "${word}"`);
          return;
        }
      }
      showStatus("No definition found");
    } else {
      showStatus("No definition found");
    }
  } finally {
    lspNavigationInFlight = false;
  }
}

// 按文件各自防抖:若全局共用一个 timer,700ms 内编辑/切换到另一个文件会把
// 上一个文件待发的 didChange 取消掉,LSP 里留下过期内容(诊断/跳转随之错乱)。
const lspChangeTimers = new Map<string, ReturnType<typeof setTimeout>>();

export function debouncedLspDidChange(path: string, getContent: () => string) {
  const existing = lspChangeTimers.get(path);
  if (existing) clearTimeout(existing);
  lspChangeTimers.set(path, setTimeout(() => {
    lspChangeTimers.delete(path);
    lspDidChange(path, getContent()).catch(() => {});
  }, 700));
}
