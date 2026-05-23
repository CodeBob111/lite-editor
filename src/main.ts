import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
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
import { oneDark } from "@codemirror/theme-one-dark";
import { java } from "@codemirror/lang-java";
import { python } from "@codemirror/lang-python";
import { javascript } from "@codemirror/lang-javascript";

import { FileTree } from "./file-tree";
import { TabManager } from "./tabs";
import { PanelManager } from "./panel";

function getLanguageExtension(filename: string) {
  if (filename.endsWith(".java")) return java();
  if (filename.endsWith(".py")) return python();
  if (filename.endsWith(".ts") || filename.endsWith(".tsx")) return javascript({ typescript: true, jsx: true });
  if (filename.endsWith(".js") || filename.endsWith(".jsx")) return javascript({ jsx: true });
  return [];
}

function createEditorState(content: string, filename: string): EditorState {
  return EditorState.create({
    doc: content,
    extensions: [
      history(),
      foldGutter(),
      bracketMatching(),
      highlightSelectionMatches(),
      autocompletion(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      keymap.of([
        ...defaultKeymap,
        ...historyKeymap,
        ...searchKeymap,
        ...completionKeymap,
        ...foldKeymap,
      ]),
      oneDark,
      EditorView.lineWrapping,
      getLanguageExtension(filename),
    ],
  });
}

const editorContainer = document.getElementById("editor-container")!;
let editorView: EditorView | null = null;

const tabManager = new TabManager(
  document.getElementById("tabs-bar")!,
  (filePath, content) => {
    if (editorView) {
      editorView.destroy();
    }
    const state = createEditorState(content, filePath);
    editorView = new EditorView({ state, parent: editorContainer });
    fileTree.highlightFile(filePath);
  }
);

const fileTree = new FileTree(
  document.getElementById("file-tree")!,
  (filePath) => {
    tabManager.openFile(filePath);
  }
);

const panelManager = new PanelManager(
  document.getElementById("panel-tabs")!,
  document.getElementById("panel-content")!
);

document.getElementById("btn-locate-file")!.addEventListener("click", () => {
  const currentFile = tabManager.getActiveFile();
  if (currentFile) {
    fileTree.revealFile(currentFile);
  }
});

const welcomeContent = `// Welcome to Lite Editor
//
// A lightweight code editor built with Tauri + CodeMirror 6
//
// Features:
//   - Find Usages (Shift+F12)
//   - Scroll to Source (sidebar target icon)
//   - Maven project management
//
// Open a folder to get started: File > Open Folder
`;

const state = createEditorState(welcomeContent, "welcome.ts");
editorView = new EditorView({ state, parent: editorContainer });
