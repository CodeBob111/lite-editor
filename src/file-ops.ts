import { EditorView } from "@codemirror/view";
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

// openFileAtLine 自己会精确记录(含目标行号、同文件跳转),它驱动的标签激活期间要抑制
// onTabActivate 里的兜底记录,避免同一次跳转被记两遍。
let suppressTabNavRecord = false;

// 切换/打开标签页时记录导航历史(IntelliJ 式 cmd+[ / cmd+])。先记离开位置(带最新光标行),
// 再记落点 —— 这样「后退」能回到刚才所在的行。openFileAtLine 驱动的激活已被抑制。
export function navRecordTabSwitch(prevFile: string | null, prevLine: number, newFile: string, newLine: number) {
  if (suppressTabNavRecord) return;
  if (prevFile && prevFile !== newFile) navPush({ file: prevFile, line: prevLine });
  navPush({ file: newFile, line: newLine });
}

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
      // 跳转到同文件内其它行时,把目标行居中(默认 scrollIntoView:true 只滚到最近边缘,
      // 落在最上或最下)。与新开/切标签页路径(pendingScrollLine)的居中行为保持一致。
      effects: EditorView.scrollIntoView(targetLine.from, { y: "center" }),
    });
    flashLine(app.editorView, clampedLine);
    if (!isNavRestore) {
      navPush({ file: filePath, line });
    }
    return;
  }

  if (tabManager.hasTab(filePath)) {
    app.pendingScrollLine = line;
    suppressTabNavRecord = true;
    try { tabManager.activatePath(filePath); } finally { suppressTabNavRecord = false; }
    if (!isNavRestore) {
      navPush({ file: filePath, line });
    }
    return;
  }

  try {
    const content = await readFile(filePath);
    app.pendingScrollLine = line;
    suppressTabNavRecord = true;
    try { tabManager.openFile(filePath, content); } finally { suppressTabNavRecord = false; }
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
