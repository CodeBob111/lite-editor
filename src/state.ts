import type { EditorView } from "@codemirror/view";
import type { FileNode } from "./file-tree";
import type { OpenTab } from "./tabs";
import type { LspDiagnosticParams } from "./tauri-api";

export interface ProjectState {
  path: string;
  name: string;
  treeRoot: FileNode | null;
  tabs: OpenTab[];
  activeTabIndex: number;
  activeFilePath: string | null;
  allFilePathsCache: string[] | null;
}

export interface PersistedSession {
  version: number;
  projects: { path: string; openFiles: string[]; activeFile: string | null }[];
  activeProjectIndex: number;
}

const EDITOR_CACHE_MAX = 30;
const DIAGNOSTICS_MAX = 200;

class LruEditorCache {
  private map = new Map<string, EditorView>();
  private order: string[] = [];

  get(key: string): EditorView | undefined {
    const v = this.map.get(key);
    if (v !== undefined) {
      const i = this.order.indexOf(key);
      if (i > -1) this.order.splice(i, 1);
      this.order.push(key);
    }
    return v;
  }

  set(key: string, view: EditorView) {
    if (this.map.has(key)) {
      const old = this.map.get(key);
      if (old && old !== view) old.destroy();
      const i = this.order.indexOf(key);
      if (i > -1) this.order.splice(i, 1);
    }
    this.map.set(key, view);
    this.order.push(key);
    while (this.order.length > EDITOR_CACHE_MAX) {
      const oldest = this.order.shift()!;
      const old = this.map.get(oldest);
      if (old) { old.destroy(); this.map.delete(oldest); }
    }
  }

  delete(key: string) {
    const v = this.map.get(key);
    if (v) { v.destroy(); this.map.delete(key); }
    const i = this.order.indexOf(key);
    if (i > -1) this.order.splice(i, 1);
  }

  has(key: string): boolean { return this.map.has(key); }
  get size(): number { return this.map.size; }
  values(): IterableIterator<EditorView> { return this.map.values(); }
  keys(): IterableIterator<string> { return this.map.keys(); }

  [Symbol.iterator](): IterableIterator<[string, EditorView]> {
    return this.map[Symbol.iterator]();
  }

  clear() {
    for (const v of this.map.values()) v.destroy();
    this.map.clear();
    this.order = [];
  }
}

class BoundedDiagnosticsMap {
  private map = new Map<string, LspDiagnosticParams["diagnostics"]>();

  get(key: string) { return this.map.get(key); }

  set(key: string, value: LspDiagnosticParams["diagnostics"]) {
    this.map.set(key, value);
    if (this.map.size > DIAGNOSTICS_MAX) {
      const first = this.map.keys().next().value;
      if (first !== undefined) this.map.delete(first);
    }
  }

  delete(key: string) { return this.map.delete(key); }
  has(key: string) { return this.map.has(key); }
  keys() { return this.map.keys(); }
  clear() { this.map.clear(); }
}

export const app = {
  projects: [] as ProjectState[],
  activeProjectIndex: -1,
  currentProjectPath: null as string | null,
  currentFilePath: null as string | null,
  editorView: null as EditorView | null,
  editorViewCache: new LruEditorCache(),
  // Last content we wrote to disk per path; lets the file watcher distinguish
  // an external change from the echo of our own autosave.
  savedContentCache: new Map<string, string>(),
  diagnosticsMap: new BoundedDiagnosticsMap(),
  isRestoring: true,
  javaIndexReady: false,
  pendingScrollLine: null as number | null,
  saveSessionTimeout: null as ReturnType<typeof setTimeout> | null,
};

export function currentProject(): ProjectState | null {
  return app.activeProjectIndex >= 0 && app.activeProjectIndex < app.projects.length
    ? app.projects[app.activeProjectIndex]
    : null;
}

export function destroyAllCachedViews() {
  app.editorViewCache.clear();
  app.savedContentCache.clear();
}

export function destroyCachedView(path: string) {
  app.editorViewCache.delete(path);
  app.savedContentCache.delete(path);
}

export function destroyCachedViewsByPrefix(prefix: string) {
  const toDelete: string[] = [];
  for (const p of app.editorViewCache.keys()) {
    if (p === prefix || p.startsWith(prefix + "/")) toDelete.push(p);
  }
  for (const p of toDelete) app.editorViewCache.delete(p);
  for (const p of [...app.savedContentCache.keys()]) {
    if (p === prefix || p.startsWith(prefix + "/")) app.savedContentCache.delete(p);
  }
}
