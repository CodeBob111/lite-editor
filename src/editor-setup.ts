import { EditorState } from "@codemirror/state";
import { EditorView, keymap, ViewUpdate } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import { autocompletion, completionKeymap } from "@codemirror/autocomplete";
import {
  syntaxHighlighting,
  defaultHighlightStyle,
  bracketMatching,
  foldGutter,
  foldKeymap,
} from "@codemirror/language";
import { linter, lintGutter, type Diagnostic } from "@codemirror/lint";

import { warmEarthTheme } from "./editor-theme";
import { flashLineField } from "./flash-line";
import { blameExtensions } from "./git-blame-gutter";
import { languageCompartment } from "./editor-language";
import { showEditorContextMenu } from "./editor-context-menu";
import { navigateBack, navigateForward } from "./file-ops";
import { smartNavigateAtPos, debouncedLspDidChange } from "./lsp-navigation";
import { app } from "./state";
import { showStatus } from "./utils";
import { writeFile } from "./tauri-api";
import { loadChanges } from "./changes-panel";
import type { TabManager } from "./tabs";

let _tabManager: TabManager;

export function initEditorSetup(tabManager: TabManager) {
  _tabManager = tabManager;
}

function diagnosticSource(view: EditorView): Diagnostic[] {
  if (!app.currentFilePath) return [];
  const uri = `file://${app.currentFilePath}`;
  const diags = app.diagnosticsMap.get(uri);
  if (!diags || diags.length === 0) return [];

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

  return result;
}

export function createEditorState(content: string, filename: string): EditorState {
  return EditorState.create({
    doc: content,
    extensions: [
      history(),
      foldGutter(),
      bracketMatching(),
      highlightSelectionMatches(),
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
      EditorView.lineWrapping,
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
    _tabManager.markSaved(app.currentFilePath);
    showStatus(`Saved ${app.currentFilePath.split("/").pop()}`);
    const changesActive = document.querySelector('.panel-tab[data-panel="changes"].active');
    if (changesActive) loadChanges();
  } catch (e) {
    showStatus(`Save failed: ${e}`, true);
  }
}
