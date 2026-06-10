import { EditorState, Annotation, Transaction } from "@codemirror/state";
import { EditorView, keymap, ViewUpdate, lineNumbers, tooltips } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { search, searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import { autocompletion, completionKeymap } from "@codemirror/autocomplete";
import {
  syntaxHighlighting,
  defaultHighlightStyle,
  foldKeymap,
} from "@codemirror/language";
import { linter, lintGutter, type Diagnostic } from "@codemirror/lint";

import { warmEarthTheme } from "./editor-theme";
import { createSearchPanel } from "./editor-search-panel";
import { flashLineField } from "./flash-line";
import { blameExtensions } from "./git-blame-gutter";
import { languageCompartment } from "./editor-language";
import { showEditorContextMenu } from "./editor-context-menu";
import { navigateBack, navigateForward } from "./file-ops";
import { smartNavigateAtPos, debouncedLspDidChange } from "./lsp-navigation";
import { app } from "./state";
import { showStatus } from "./utils";
import { updateStatusCursor, setStatusDiagnostics } from "./status-bar";
import { editorSettingsExtensions } from "./settings";
import { writeFile, updateUsageIndexFile } from "./tauri-api";
import { loadChanges } from "./changes-panel";
import type { TabManager } from "./tabs";

let _tabManager: TabManager;

export function initEditorSetup(tabManager: TabManager) {
  _tabManager = tabManager;
}

// Marks a transaction as a programmatic reload of externally-changed disk
// content, so the updateListener below skips marking the tab dirty / autosaving
// / re-notifying the LSP for it. Without this, reloading the editor would look
// like a user edit and immediately overwrite the disk change back.
export const externalReload = Annotation.define<boolean>();

function diagnosticSource(view: EditorView): Diagnostic[] {
  if (!app.currentFilePath) { setStatusDiagnostics(0, 0); return []; }
  const uri = `file://${app.currentFilePath}`;
  const diags = app.diagnosticsMap.get(uri);
  if (!diags || diags.length === 0) { setStatusDiagnostics(0, 0); return []; }

  const doc = view.state.doc;
  const result: Diagnostic[] = [];

  for (const d of diags) {
    const startLine = Math.min(d.range.start.line + 1, doc.lines);
    const endLine = Math.min(d.range.end.line + 1, doc.lines);
    const sl = doc.line(startLine);
    const el = doc.line(endLine);
    const from = Math.min(sl.from + d.range.start.character, sl.to);
    const to = Math.min(el.from + d.range.end.character, el.to);

    let severity: "error" | "warning" | "info" = "info";
    if (d.severity === 1) severity = "error";
    else if (d.severity === 2) severity = "warning";

    result.push({
      from: Math.max(from, 0),
      to: Math.max(to, from),
      severity,
      message: d.message,
      source: d.source,
    });
  }

  setStatusDiagnostics(
    result.filter((r) => r.severity === "error").length,
    result.filter((r) => r.severity === "warning").length,
  );
  return result;
}

export function createEditorState(content: string, filename: string): EditorState {
  return EditorState.create({
    doc: content,
    extensions: [
      lineNumbers(),
      history(),
      ...editorSettingsExtensions(),
      highlightSelectionMatches(),
      // 把 Cmd+F 查找面板放到编辑器顶部(默认在底部),并用自定义面板做成 IDEA 同款
      // 查找条(见 editor-search-panel.ts)。
      search({ top: true, createPanel: createSearchPanel }),
      autocompletion(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      linter(diagnosticSource),
      lintGutter(),
      // 用 absolute 定位 tooltip:祖先 #main 有 contain:strict(含 layout),会让默认的
      // fixed 定位先落错位置、再被测量修正,表现为弹框出现后「先跳一下」。改 absolute 即可消除。
      tooltips({ position: "absolute" }),
      keymap.of([
        { key: "Mod-[", run: () => { navigateBack(); return true; } },
        { key: "Mod-]", run: () => { navigateForward(); return true; } },
        ...defaultKeymap,
        ...historyKeymap,
        ...searchKeymap,
        ...completionKeymap,
        ...foldKeymap,
      ]),
      ...warmEarthTheme,
      flashLineField,
      languageCompartment.of([]),
      ...blameExtensions,
      EditorView.domEventHandlers({
        mousedown(e, view) {
          if (e.metaKey && !e.shiftKey && !e.altKey && e.button === 0) {
            const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
            if (pos !== null) {
              e.preventDefault();
              view.dispatch({ selection: { anchor: pos } });
              smartNavigateAtPos(view, pos);
              return true;
            }
          }
          return false;
        },
        contextmenu(e, view) {
          if (!app.currentFilePath || !app.currentProjectPath) return false;
          e.preventDefault();
          showEditorContextMenu(view, e.clientX, e.clientY);
          return true;
        },
      }),
      EditorView.updateListener.of((update: ViewUpdate) => {
        if (update.selectionSet || update.docChanged) updateStatusCursor();
        if (update.transactions.some((tr) => tr.annotation(externalReload))) return;
        if (update.docChanged && app.currentFilePath) {
          _tabManager.markDirty(app.currentFilePath);
          const filePath = app.currentFilePath;
          const state = update.state;
          debouncedLspDidChange(filePath, () => state.doc.toString());
          debouncedAutoSave(filePath, update.view);
        }
      }),
    ],
  });
}

// 自动保存也按文件各自防抖,并在调度时锁定 path+view:全局单 timer + 保存「当前文件」
// 的旧写法下,编辑 A 后 1s 内切到 B 再编辑,A 的待存改动会被取消——A 一直 dirty,
// 一旦它的视图被 LRU 淘汰销毁,这些改动就悄悄丢了。
const autoSaveTimers = new Map<string, ReturnType<typeof setTimeout>>();

function debouncedAutoSave(path: string, view: EditorView) {
  const existing = autoSaveTimers.get(path);
  if (existing) clearTimeout(existing);
  autoSaveTimers.set(path, setTimeout(() => {
    autoSaveTimers.delete(path);
    saveFile(path, view);
  }, 1000));
}

// 关闭标签 = 用户选择丢弃未保存改动:必须取消该文件待触发的自动保存。
// 只在触发时查 hasTab 不够——关闭后 1s 内又重新打开同一文件,hasTab 为 true,
// 旧 view 里被丢弃的内容会被写回磁盘,再经文件监听 reload 回灌进新编辑器。
export function cancelPendingAutoSave(path: string) {
  const timer = autoSaveTimers.get(path);
  if (timer) {
    clearTimeout(timer);
    autoSaveTimers.delete(path);
  }
}

async function saveFile(path: string, view: EditorView) {
  const content = view.state.doc.toString();
  try {
    await writeFile(path, content);
    // Remember what we just wrote so the file watcher's echo of our own save
    // isn't mistaken for an external change while the user keeps typing.
    app.savedContentCache.set(path, content);
    _tabManager.markSaved(path);
    showStatus(`Saved ${path.split("/").pop()}`);
    // 保存后增量更新「符号出现」倒排索引(只重扫这一个文件、改内存,很轻),
    // 让 find-usages 立刻反映刚改的内容。
    if (app.currentProjectPath && path.endsWith(".java")) {
      updateUsageIndexFile(app.currentProjectPath, path).catch(() => {});
    }
    const changesActive = document.getElementById("commit-view")?.classList.contains("active");
    if (changesActive) loadChanges();
  } catch (e) {
    showStatus(`Save failed: ${e}`, true);
  }
}

export async function saveCurrentFile() {
  if (!app.currentFilePath || !app.editorView) return;
  await saveFile(app.currentFilePath, app.editorView);
}

/**
 * Replace a view's whole document with externally-changed disk content without
 * tripping the autosave/dirty/LSP updateListener (the externalReload annotation
 * makes that listener skip this transaction). The cursor is clamped into the new
 * document so a shorter reloaded file doesn't throw a stale out-of-range head.
 * Works on detached cached views too (their DOM is re-attached on tab switch).
 *
 * addToHistory:false keeps the reload OUT of the undo stack — otherwise a single
 * Cmd-Z after a reload would revert to the pre-reload content, mark the tab dirty,
 * and let autosave silently overwrite the external change back onto disk.
 */
export function applyExternalContent(view: EditorView, content: string) {
  const head = Math.min(view.state.selection.main.head, content.length);
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: content },
    selection: { anchor: head },
    annotations: [externalReload.of(true), Transaction.addToHistory.of(false)],
  });
}
