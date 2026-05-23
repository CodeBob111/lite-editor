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
import { oneDark } from "@codemirror/theme-one-dark";
import { java } from "@codemirror/lang-java";
import { python } from "@codemirror/lang-python";
import { javascript } from "@codemirror/lang-javascript";

import { FileTree, type FileNode } from "./file-tree";
import { TabManager } from "./tabs";
import { PanelManager } from "./panel";
import {
  openFolderDialog,
  readDirTree,
  readFile,
  writeFile,
  toFrontendNode,
  parseMavenModules,
  runMavenCommand,
  startLsp,
  lspDidOpen,
  lspDidChange,
  lspFindReferences,
  lspGotoDefinition,
  type LspUsage,
} from "./tauri-api";

// ---- State ----

let currentProjectPath: string | null = null;
let currentFilePath: string | null = null;
let editorView: EditorView | null = null;
let saveTimeout: ReturnType<typeof setTimeout> | null = null;

// ---- Language detection ----

function getLanguageExtension(filename: string) {
  if (filename.endsWith(".java")) return java();
  if (filename.endsWith(".py")) return python();
  if (filename.endsWith(".ts") || filename.endsWith(".tsx"))
    return javascript({ typescript: true, jsx: true });
  if (filename.endsWith(".js") || filename.endsWith(".jsx"))
    return javascript({ jsx: true });
  return [];
}

function getLanguageId(filename: string): string {
  if (filename.endsWith(".java")) return "java";
  if (filename.endsWith(".py")) return "python";
  if (filename.endsWith(".ts") || filename.endsWith(".tsx")) return "typescript";
  if (filename.endsWith(".js") || filename.endsWith(".jsx")) return "javascript";
  return "plaintext";
}

// ---- Editor creation ----

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
        // Cmd+S / Ctrl+S → save
        {
          key: "Mod-s",
          run: () => {
            saveCurrentFile();
            return true;
          },
        },
        // Shift+F12 → Find Usages
        {
          key: "Shift-F12",
          run: (view) => {
            findUsagesAtCursor(view);
            return true;
          },
        },
        // F12 → Go to Definition
        {
          key: "F12",
          run: (view) => {
            gotoDefinitionAtCursor(view);
            return true;
          },
        },
      ]),
      oneDark,
      EditorView.lineWrapping,
      getLanguageExtension(filename),
      // Track changes for LSP didChange
      EditorView.updateListener.of((update: ViewUpdate) => {
        if (update.docChanged && currentFilePath) {
          debouncedLspDidChange(currentFilePath, update.state.doc.toString());
        }
      }),
    ],
  });
}

// ---- Save ----

async function saveCurrentFile() {
  if (!currentFilePath || !editorView) return;
  const content = editorView.state.doc.toString();
  try {
    await writeFile(currentFilePath, content);
    tabManager.markSaved(currentFilePath);
    showStatus(`Saved ${currentFilePath.split("/").pop()}`);
  } catch (e) {
    showStatus(`Save failed: ${e}`, true);
  }
}

// ---- LSP helpers ----

function debouncedLspDidChange(path: string, content: string) {
  if (saveTimeout) clearTimeout(saveTimeout);
  saveTimeout = setTimeout(() => {
    lspDidChange(path, content).catch(() => {});
  }, 300);
}

async function findUsagesAtCursor(view: EditorView) {
  if (!currentFilePath) return;
  const pos = view.state.selection.main.head;
  const line = view.state.doc.lineAt(pos);
  const lineNumber = line.number - 1; // LSP is 0-indexed
  const character = pos - line.from; // offset within line

  showStatus("Finding usages...");
  try {
    const usages = await lspFindReferences(currentFilePath, lineNumber, character);
    const word = getWordAtPos(view, pos);
    panelManager.showUsages(
      word,
      usages.map((u) => ({
        file: u.uri.replace("file://", ""),
        line: u.line + 1, // display as 1-indexed
        text: u.text,
      })),
      (file, line) => openFileAtLine(file, line)
    );
    showStatus(`${usages.length} usage(s) found`);
  } catch (e) {
    showStatus(`Find Usages: ${e}`, true);
  }
}

async function gotoDefinitionAtCursor(view: EditorView) {
  if (!currentFilePath) return;
  const pos = view.state.selection.main.head;
  const line = view.state.doc.lineAt(pos);
  const lineNumber = line.number - 1;
  const character = pos - line.from;

  showStatus("Going to definition...");
  try {
    const def = await lspGotoDefinition(currentFilePath, lineNumber, character);
    if (def) {
      const file = def.uri.replace("file://", "");
      openFileAtLine(file, def.line + 1);
      showStatus(`Jumped to ${file.split("/").pop()}:${def.line + 1}`);
    } else {
      showStatus("No definition found");
    }
  } catch (e) {
    showStatus(`Go to Definition: ${e}`, true);
  }
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

// ---- File operations ----

async function openFileAtLine(filePath: string, line: number) {
  try {
    const content = await readFile(filePath);
    tabManager.openFile(filePath, content);
    // Scroll to line after editor is created
    setTimeout(() => {
      if (editorView) {
        const targetLine = editorView.state.doc.line(Math.min(line, editorView.state.doc.lines));
        editorView.dispatch({
          selection: { anchor: targetLine.from },
          scrollIntoView: true,
        });
      }
    }, 50);
  } catch (e) {
    showStatus(`Failed to open ${filePath}: ${e}`, true);
  }
}

async function openFile(filePath: string) {
  try {
    const content = await readFile(filePath);
    tabManager.openFile(filePath, content);
  } catch (e) {
    showStatus(`Failed to open ${filePath}: ${e}`, true);
  }
}

// ---- Project open ----

async function openProject(folderPath: string) {
  currentProjectPath = folderPath;
  showStatus(`Opening ${folderPath}...`);

  try {
    const tree = await readDirTree(folderPath);
    const frontendTree = toFrontendNode(tree);
    fileTree.setRoot(frontendTree);
    showStatus(`Opened ${frontendTree.name}`);

    // Update window title
    document.title = `${frontendTree.name} — Lite Editor`;

    // Auto-detect and start LSP servers
    autoStartLsp(folderPath);

    // Load Maven modules if applicable
    loadMavenModules(folderPath);
  } catch (e) {
    showStatus(`Failed to open folder: ${e}`, true);
  }
}

async function autoStartLsp(rootPath: string) {
  // Try starting language servers — failures are silent (server may not be installed)
  const attempts = [
    { lang: "python", check: ".py" },
    { lang: "typescript", check: ".ts" },
  ];

  for (const { lang } of attempts) {
    startLsp(lang, rootPath).catch(() => {
      // Server not installed, that's fine
    });
  }
}

async function loadMavenModules(projectPath: string) {
  try {
    const modules = await parseMavenModules(projectPath);
    if (modules.length > 0) {
      panelManager.showMavenModules(
        modules.map((m) => ({
          name: m.name,
          groupId: m.group_id,
          artifactId: m.artifact_id,
        }))
      );
      // Wire up maven action buttons
      document.querySelectorAll(".maven-action").forEach((btn) => {
        btn.addEventListener("click", async (e) => {
          const target = e.target as HTMLElement;
          const cmd = target.dataset.cmd!;
          const mod = target.dataset.module!;
          // Find the module's pom directory
          const module = modules.find((m) => m.artifact_id === mod);
          if (module && currentProjectPath) {
            const pomDir = module.pom_path.replace(/\/pom\.xml$/, "");
            showStatus(`Running mvn ${cmd} on ${mod}...`);
            try {
              const output = await runMavenCommand(pomDir, [cmd]);
              panelManager.showMavenOutput(mod, cmd, output);
              showStatus(`mvn ${cmd} completed`);
            } catch (e) {
              panelManager.showMavenOutput(mod, cmd, `ERROR:\n${e}`);
              showStatus(`mvn ${cmd} failed`, true);
            }
          }
        });
      });
    }
  } catch {
    // No Maven project, that's fine
  }
}

// ---- Status bar ----

function showStatus(message: string, isError = false) {
  const el = document.getElementById("status-text")!;
  el.textContent = message;
  el.style.color = isError ? "#f38ba8" : "var(--text-muted)";
  if (!isError) {
    setTimeout(() => {
      if (el.textContent === message) el.textContent = "Ready";
    }, 3000);
  }
}

// ---- Wire up components ----

const editorContainer = document.getElementById("editor-container")!;

const tabManager = new TabManager(
  document.getElementById("tabs-bar")!,
  (filePath, content) => {
    currentFilePath = filePath;
    if (editorView) {
      editorView.destroy();
    }
    const state = createEditorState(content, filePath);
    editorView = new EditorView({ state, parent: editorContainer });

    // Notify LSP about the opened file
    const langId = getLanguageId(filePath);
    lspDidOpen(filePath, langId, content).catch(() => {});

    fileTree.highlightFile(filePath);
  }
);

const fileTree = new FileTree(
  document.getElementById("file-tree")!,
  (filePath) => openFile(filePath)
);

const panelManager = new PanelManager(
  document.getElementById("panel-tabs")!,
  document.getElementById("panel-content")!
);

// ---- Toolbar buttons ----

document.getElementById("btn-open-folder")!.addEventListener("click", async () => {
  const folder = await openFolderDialog();
  if (folder) {
    openProject(folder);
  }
});

document.getElementById("btn-locate-file")!.addEventListener("click", () => {
  const current = tabManager.getActiveFile();
  if (current) {
    fileTree.revealFile(current);
  }
});

// ---- Welcome state ----

const welcomeContent = `// Welcome to Lite Editor
//
// A lightweight code editor built with Tauri + CodeMirror 6
//
// Shortcuts:
//   Cmd+S         — Save file
//   Shift+F12     — Find Usages (LSP)
//   F12           — Go to Definition (LSP)
//
// Click "Open Folder" in the sidebar to get started.
`;

const state = createEditorState(welcomeContent, "welcome.ts");
editorView = new EditorView({ state, parent: editorContainer });

showStatus("Ready");
