import { EditorState, Annotation, Transaction } from "@codemirror/state";
import { EditorView, keymap, ViewUpdate } from "@codemirror/view";
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
import { writeFile } from "./tauri-api";
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
          debouncedAutoSave();
        }
      }),
    ],
  });
}

let autoSaveTimer: ReturnType<typeof setTimeout> | null = null;
function debouncedAutoSave() {
  if (autoSaveTimer) clearTimeout(autoSaveTimer);
  autoSaveTimer = setTimeout(() => saveCurrentFile(), 1000);
}

export async function saveCurrentFile() {
  if (!app.currentFilePath || !app.editorView) return;
  const content = app.editorView.state.doc.toString();
  try {
    await writeFile(app.currentFilePath, content);
    // Remember what we just wrote so the file watcher's echo of our own save
    // isn't mistaken for an external change while the user keeps typing.
    app.savedContentCache.set(app.currentFilePath, content);
    _tabManager.markSaved(app.currentFilePath);
    showStatus(`Saved ${app.currentFilePath.split("/").pop()}`);
    const changesActive = document.getElementById("commit-view")?.classList.contains("active");
    if (changesActive) loadChanges();
  } catch (e) {
    showStatus(`Save failed: ${e}`, true);
  }
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
