import { app } from "./state";
import { readFile } from "./tauri-api";
import { showStatus } from "./utils";
import { flashLine } from "./flash-line";
import type { TabManager } from "./tabs";

let tabManager: TabManager;

export function initFileOps(tm: TabManager) {
  tabManager = tm;
}

// ---- Navigation history ----

interface NavLocation { file: string; line: number; }
const navHistory: NavLocation[] = [];
let navIndex = -1;

export function navPush(loc: NavLocation) {
  if (navIndex >= 0) {
    const prev = navHistory[navIndex];
    if (prev.file === loc.file && prev.line === loc.line) return;
  }
  navHistory.splice(navIndex + 1);
  navHistory.push(loc);
  if (navHistory.length > 100) { navHistory.shift(); }
  navIndex = navHistory.length - 1;
}

export function navigateBack() {
  if (navIndex <= 0) return;
  navIndex--;
  openFileAtLine(navHistory[navIndex].file, navHistory[navIndex].line, true);
}

export function navigateForward() {
  if (navIndex >= navHistory.length - 1) return;
  navIndex++;
  openFileAtLine(navHistory[navIndex].file, navHistory[navIndex].line, true);
}

// ---- File operations ----

export async function openFileAtLine(filePath: string, line: number, isNavRestore = false) {
  if (!isNavRestore && app.currentFilePath && app.editorView) {
    const curLine = app.editorView.state.doc.lineAt(app.editorView.state.selection.main.head).number;
    navPush({ file: app.currentFilePath, line: curLine });
  }

  if (filePath === app.currentFilePath && app.editorView) {
    const clampedLine = Math.min(line, app.editorView.state.doc.lines);
    const targetLine = app.editorView.state.doc.line(clampedLine);
    app.editorView.dispatch({
      selection: { anchor: targetLine.from },
      scrollIntoView: true,
    });
    flashLine(app.editorView, clampedLine);
    if (!isNavRestore) {
      navPush({ file: filePath, line });
    }
    return;
  }

  if (tabManager.hasTab(filePath)) {
    app.pendingScrollLine = line;
    tabManager.activatePath(filePath);
    if (!isNavRestore) {
      navPush({ file: filePath, line });
    }
    return;
  }

  try {
    const content = await readFile(filePath);
    app.pendingScrollLine = line;
    tabManager.openFile(filePath, content);
    if (!isNavRestore) {
      navPush({ file: filePath, line });
    }
  } catch (e) {
    showStatus(`Failed to open ${filePath}: ${e}`, true);
  }
}

export async function openFile(filePath: string) {
  if (filePath === app.currentFilePath) return;
  if (tabManager.activatePath(filePath)) return;

  try {
    const content = await readFile(filePath);
    tabManager.openFile(filePath, content);
  } catch (e) {
    showStatus(`Failed to open ${filePath}: ${e}`, true);
  }
}
